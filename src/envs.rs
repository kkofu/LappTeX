use std::collections::HashMap;

use crate::refs::sanitize_label_key;
use crate::tex::{apply_edits, find_balanced, find_bracket, is_name_char, parse_braced, skip_bracket_and_braced, skip_brackets, skip_ws};

/// Remove whole `\begin{env}...\end{env}` blocks including content.
/// Handles nesting of the same env. Returns (text, dropped_count).
pub(crate) fn drop_env(body: &str, env: &str) -> (String, usize) {
    let begin = format!("\\begin{{{env}}}");
    if !body.contains(&begin) {
        return (body.to_string(), 0);
    }
    let end = format!("\\end{{{env}}}");
    let mut out = String::with_capacity(body.len());
    let mut i = 0;
    let b = body.as_bytes();
    let mut dropped = 0;
    while i < b.len() {
        if body[i..].starts_with(&begin) {
            let after = i + begin.len();
            let next = b.get(after).copied().unwrap_or(b' ');
            if is_name_char(next) {
                out.push(body[i..].chars().next().unwrap());
                i += 1;
                continue;
            }
            let j = skip_bracket_and_braced(body, after);
            // Depth-count nested same-env blocks.
            let mut depth = 1;
            let mut k = j;
            let mut end_pos: Option<usize> = None;
            while k < b.len() {
                if body[k..].starts_with(&begin) {
                    depth += 1;
                    k += begin.len();
                } else if body[k..].starts_with(&end) {
                    depth -= 1;
                    k += end.len();
                    if depth == 0 {
                        end_pos = Some(k);
                        break;
                    }
                } else {
                    let ch = body[k..].chars().next().unwrap();
                    k += ch.len_utf8();
                }
            }
            match end_pos {
                Some(e) => {
                    dropped += 1;
                    i = e;
                }
                None => {
                    // Unclosed: copy through (mitex will report it).
                    out.push_str(&body[i..j]);
                    i = j;
                }
            }
            continue;
        }
        let ch = body[i..].chars().next().unwrap();
        out.push(ch);
        i += ch.len_utf8();
    }
    (out, dropped)
}

/// Extract `algorithm` envs: drop the pseudocode body, keep the caption as
/// `Algorithm N.` header text. Labels inside map to static numbers.
/// Returns (text, label -> ("Algorithm", number)).
pub(crate) fn extract_algorithms(body: &str) -> (String, HashMap<String, (String, String)>) {
    let begin = "\\begin{algorithm}";
    if !body.contains(begin) {
        return (body.to_string(), HashMap::new());
    }
    let mut map = HashMap::new();
    let mut edits: Vec<(usize, usize, String)> = Vec::new();
    let mut count = 0;
    let mut i = 0;
    let b = body.as_bytes();
    let end = "\\end{algorithm}";
    while let Some(pos) = body[i..].find(begin) {
        let abs = i + pos;
        let after = abs + begin.len();
        let next = b.get(after).copied().unwrap_or(b' ');
        if is_name_char(next) {
            i = after;
            continue;
        }
        let j = skip_bracket_and_braced(body, after);
        let inner_end = match body[j..].find(end) {
            Some(p) => j + p,
            None => {
                i = after;
                continue;
            }
        };
        let inner = &body[j..inner_end];
        // First \caption{...} (skip [...] short caption). The label may sit
        // inside the caption braces or immediately after the command.
        let mut captext = String::new();
        let mut cap_end_rel = 0usize; // end of caption command, relative to inner
        if let Some(cp) = inner.find("\\caption") {
            let cafter = cp + "\\caption".len();
            let cj = skip_brackets(inner, cafter);
            if let Some((arg, next)) = parse_braced(inner, cj) {
                captext = arg.to_string();
                cap_end_rel = next;
            }
        }
        let mut label_key: Option<String> = None;
        let mut label_span: Option<(usize, usize)> = None; // absolute coords
        // (a) label inside the caption text.
        if !captext.is_empty() {
            if let Some(lp) = captext.find("\\label") {
                let lafter = lp + "\\label".len();
                if let Some((arg, lend)) = parse_braced(&captext, lafter) {
                    label_key = Some(arg.trim().to_string());
                    captext = format!("{}{}", &captext[..lp], &captext[lend..]);
                }
            }
        }
        // (b) label right after the caption command.
        if label_key.is_none() && cap_end_rel > 0 {
            let after_cap = &inner[cap_end_rel..];
            let trimmed = after_cap.trim_start();
            if trimmed.starts_with("\\label") {
                let lp_abs = cap_end_rel + (after_cap.len() - trimmed.len());
                if let Some((arg, lend)) =
                    parse_braced(inner, lp_abs + "\\label".len())
                {
                    label_key = Some(arg.trim().to_string());
                    label_span = Some((j + lp_abs, j + lend));
                }
            }
        }
        count += 1;
        let num = count.to_string();
        let mut header = format!("\n\n\\textbf{{Algorithm {num}.}} ");
        if !captext.trim().is_empty() {
            header.push_str(captext.trim());
            header.push(' ');
        }
        edits.push((abs, inner_end + end.len(), header));
        if let Some(span) = label_span {
            edits.push((span.0, span.1, String::new()));
        }
        if let Some(key) = label_key {
            if !key.is_empty() {
                map.insert(sanitize_label_key(&key), ("Algorithm".to_string(), num));
            }
        }
        i = inner_end + end.len();
    }
    let out = apply_edits(body, &mut edits);
    (out, map)
}

pub(crate) fn rewrite_proof(body: &str) -> String {
    if !body.contains("\\begin{proof}") {
        return body.to_string();
    }
    let mut out = String::with_capacity(body.len());
    let mut i = 0;
    while i < body.len() {
        let rest = &body[i..];
        if rest.starts_with("\\begin{proof}") || rest.starts_with("\\begin{proof}[") {
            let after = i + "\\begin{proof}".len();
            let mut j = skip_ws(body, after);
            let label = if body.as_bytes().get(j) == Some(&b'[') {
                match find_bracket(body, j) {
                    Some(close) => {
                        let l = body[j + 1..close].to_string();
                        j = close + 1;
                        l
                    }
                    None => "Proof".to_string(),
                }
            } else {
                "Proof".to_string()
            };
            out.push_str(&format!("\\textit{{{label}.}} "));
            i = j;
            continue;
        }
        if rest.starts_with("\\end{proof}") {
            i += "\\end{proof}".len();
            continue;
        }
        let ch = rest.chars().next().unwrap();
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

/// Remove `\iffalse...\fi` blocks (commented-out TeX).
pub(crate) fn drop_iffalse(body: &str) -> String {
    let mut out = String::with_capacity(body.len());
    let mut i = 0;
    while let Some(pos) = body[i..].find("\\iffalse") {
        let abs = i + pos;
        let after = abs + "\\iffalse".len();
        let next = body.as_bytes().get(after).copied().unwrap_or(b' ');
        if next.is_ascii_alphanumeric() || next == b'@' {
            out.push_str(&body[i..after]);
            i = after;
            continue;
        }
        match body[after..].find("\\fi") {
            Some(p) => {
                out.push_str(&body[i..abs]);
                i = after + p + "\\fi".len();
            }
            None => {
                out.push_str(&body[i..]);
                return out;
            }
        }
    }
    out.push_str(&body[i..]);
    out
}

/// Remove `\begin{env}[..]{..}` / `\end{env}`, keep inner content.
pub(crate) fn strip_env(body: &str, env: &str) -> String {
    let begin = format!("\\begin{{{env}}}");
    if !body.contains(&begin) {
        return body.to_string();
    }
    let end = format!("\\end{{{env}}}");
    let mut out = String::with_capacity(body.len());
    let mut i = 0;
    while i < body.len() {
        let rest = &body[i..];
        if rest.starts_with(&begin) {
            i = skip_bracket_and_braced(body, i + begin.len());
            continue;
        }
        if rest.starts_with(&end) {
            i += end.len();
            continue;
        }
        let ch = rest.chars().next().unwrap();
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

/// Remove `[...]` options right after `\begin{env}`.
pub(crate) fn strip_env_options(body: &str, env: &str) -> String {
    let begin = format!("\\begin{{{env}}}");
    if !body.contains(&begin) {
        return body.to_string();
    }
    let mut out = String::with_capacity(body.len());
    let mut i = 0;
    while let Some(pos) = body[i..].find(&begin) {
        let abs = i + pos;
        out.push_str(&body[i..abs + begin.len()]);
        i = skip_brackets(body, abs + begin.len());
    }
    out.push_str(&body[i..]);
    out
}

pub(crate) fn rewrite_longtable(body: &str) -> String {
    let begin = "\\begin{longtable}";
    if !body.contains(begin) {
        return body.to_string();
    }
    let end = "\\end{longtable}";
    let mut out = String::with_capacity(body.len());
    let mut i = 0;
    while i < body.len() {
        let rest = &body[i..];
        if rest.starts_with(begin) {
            // Longtable colspecs always normalize to plain l's (mitex
            // limitation), unlike tabular which keeps simple colspecs.
            match parse_colspec(body, i + begin.len(), false) {
                Some((spec, next)) => {
                    out.push_str(&format!(
                        "\\begin{{tabular}}{{{}}}",
                        "l".repeat(count_table_cols(spec).max(1))
                    ));
                    i = next;
                }
                None => {
                    // No colspec (invalid LaTeX, but preserve old behavior).
                    let j = skip_brackets(body, i + begin.len());
                    out.push_str("\\begin{tabular}{l}");
                    i = j;
                }
            }
            continue;
        }
        if rest.starts_with(end) {
            out.push_str("\\end{tabular}");
            i += end.len();
            continue;
        }
        let ch = rest.chars().next().unwrap();
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

/// Count columns in a tabular colspec: m{}/p{}/b{}/X/L/C/R groups +
/// bare l/c/r at depth 0.
fn count_table_cols(spec: &str) -> usize {
    let mut n = 0;
    let b = spec.as_bytes();
    let mut depth: i32 = 0;
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'{' => depth += 1,
            b'}' => depth = depth.saturating_sub(1),
            b'm' | b'p' if b.get(i + 1) == Some(&b'{') => {
                n += 1;
            }
            b'b' if b.get(i + 1) == Some(&b'{') && depth == 0 => {
                // could be border spec; count conservatively only with brace
                n += 1;
            }
            b'l' | b'c' | b'r' if depth == 0 => n += 1,
            // array/tabularx extra column types (need array package in LaTeX).
            b'X' | b'L' | b'C' | b'R' if depth == 0 => n += 1,
            _ => {}
        }
        i += 1;
    }
    n
}

/// Normalize a tabular colspec for mitex: keep it only if it uses plain
/// `l/c/r` (plus whitespace); anything else (`|`, `@{...}`, `>{...}`,
/// `p{...}`, `X`, ...) is replaced with N plain `l` columns, since mitex
/// cannot parse those (notably `@{}`). Returns None when kept.
fn normalize_colspec(spec: &str) -> Option<String> {
    // Strip {...} groups, then check remaining chars.
    let mut bare = String::with_capacity(spec.len());
    let b = spec.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'{' {
            match find_balanced(spec, i) {
                Some(close) => i = close + 1,
                None => break,
            }
        } else {
            bare.push(b[i] as char);
            i += 1;
        }
    }
    let simple = bare
        .chars()
        .all(|c| matches!(c, 'l' | 'c' | 'r' | ' ' | '\t'));
    if simple && !bare.trim().is_empty() {
        None
    } else {
        Some("l".repeat(count_table_cols(spec).max(1)))
    }
}

/// After a tabular-family `\begin` tag (at `tag_end`), skip `[...]`, an
/// optional `{width}` arg, and parse the `{colspec}` group.
/// Returns (colspec_content, index_after_group).
fn parse_colspec(body: &str, tag_end: usize, width_arg: bool) -> Option<(&str, usize)> {
    let mut j = skip_brackets(body, tag_end);
    if width_arg && body.as_bytes().get(j) == Some(&b'{') {
        match find_balanced(body, j) {
            Some(close) => j = skip_ws(body, close + 1),
            None => return None,
        }
    }
    if body.as_bytes().get(j) != Some(&b'{') {
        return None;
    }
    let close = find_balanced(body, j)?;
    Some((&body[j + 1..close], close + 1))
}

/// Rewrite one tabular-family begin tag to plain `\begin{tabular}{...}`.
/// `width_arg`: whether the env takes a `{width}` arg before the colspec
/// (tabular*). Returns the index after the consumed tag.
fn rewrite_tabular_begin(
    out: &mut String,
    body: &str,
    tag_end: usize,
    width_arg: bool,
) -> Option<usize> {
    let (spec, next) = parse_colspec(body, tag_end, width_arg)?;
    let cols = normalize_colspec(spec).unwrap_or_else(|| spec.to_string());
    out.push_str(&format!("\\begin{{tabular}}{{{cols}}}"));
    Some(next)
}

/// Rewrite `tabular*` and complex-colspec `tabular` envs to plain tabulars
/// that mitex can handle. Simple colspecs (`l/c/r`, `|`, `@{...}`) are kept.
pub(crate) fn rewrite_tables(body: &str) -> String {
    if !body.contains("\\begin{tabular") {
        return body.to_string();
    }
    let mut out = String::with_capacity(body.len());
    let mut i = 0;
    let b = body.as_bytes();
    while i < b.len() {
        let rest = &body[i..];
        let mut handled = false;
        for (begin, end, width_arg) in [
            ("\\begin{tabular*}", "\\end{tabular*}", true),
            ("\\begin{tabularx}", "\\end{tabularx}", true),
            ("\\begin{tabular}", "\\end{tabular}", false),
        ] {
            if rest.starts_with(begin) {
                let after = i + begin.len();
                let next = b.get(after).copied().unwrap_or(b' ');
                if next.is_ascii_alphanumeric() || next == b'@' || next == b'*' {
                    break;
                }
                match rewrite_tabular_begin(&mut out, body, after, width_arg) {
                    Some(next_i) => i = next_i,
                    None => {
                        out.push_str(&body[i..after]);
                        i = after;
                    }
                }
                handled = true;
                break;
            }
            if rest.starts_with(end) {
                out.push_str("\\end{tabular}");
                i += end.len();
                handled = true;
                break;
            }
        }
        if handled {
            continue;
        }
        let ch = rest.chars().next().unwrap();
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

/// Flatten `subfigure` envs inside figures: inner captions become `(a) text`
/// run-ins, inner labels are dropped and mapped to their letter for static
/// references. Returns (text, label -> ("(a)", "")).
pub(crate) fn flatten_subfigures(body: &str) -> (String, HashMap<String, (String, String)>) {
    if !body.contains("\\begin{subfigure}") {
        return (body.to_string(), HashMap::new());
    }
    let mut map = HashMap::new();
    let mut edits: Vec<(usize, usize, String)> = Vec::new();
    let mut i = 0;
    // Scan figure blocks (figures don't nest).
    while let Some(pos) = body[i..].find("\\begin{figure}") {
        let fabs = i + pos;
        let fend = match body[fabs..].find("\\end{figure}") {
            Some(p) => fabs + p + "\\end{figure}".len(),
            None => break,
        };
        // Enumerate subfigures within.
        let mut idx = 0;
        let mut k = fabs;
        while let Some(sp) = body[k..fend.min(body.len())].find("\\begin{subfigure}") {
            let sabs = k + sp;
            if sabs >= fend {
                break;
            }
            let safter = sabs + "\\begin{subfigure}".len();
            let j = skip_bracket_and_braced(body, safter);
            let send = match body[j..fend].find("\\end{subfigure}") {
                Some(p) => j + p,
                None => break,
            };
            let send_end = send + "\\end{subfigure}".len();
            let inner = &body[j..send];
            let letter = ((b'a' + (idx % 26) as u8) as char).to_string();
            idx += 1;
            // Inner caption -> "(l) text".
            let mut replacement: Option<String> = None;
            let mut cap_span: Option<(usize, usize)> = None;
            if let Some(cp) = inner.find("\\caption") {
                let cafter = cp + "\\caption".len();
                let cj = skip_brackets(inner, cafter);
                if let Some((arg, next)) = parse_braced(inner, cj) {
                    // Absolute span of the whole \caption command.
                    let abs_s = j + cp;
                    let abs_e = j + next;
                    cap_span = Some((abs_s, abs_e));
                    // Label inside the caption?
                    let mut captext = arg.to_string();
                    if let Some(lp) = captext.find("\\label") {
                        let lafter = lp + "\\label".len();
                        if let Some((larg, lend)) = parse_braced(&captext, lafter) {
                            map.insert(
                                sanitize_label_key(larg.trim()),
                                (format!("({letter})"), String::new()),
                            );
                            captext = format!("{}{}", &captext[..lp], &captext[lend..]);
                        }
                    }
                    if !captext.trim().is_empty() {
                        replacement = Some(format!("({letter}) {} ", captext.trim()));
                    } else {
                        replacement = Some(String::new());
                    }
                }
            }
            // Standalone inner label (outside caption).
            if let Some(lp) = inner.find("\\label") {
                // Skip if it was inside the caption span (already handled).
                let in_cap = cap_span
                    .map(|(cs, ce)| {
                        let a = j + lp;
                        a >= cs && a < ce
                    })
                    .unwrap_or(false);
                if !in_cap {
                    let lafter = lp + "\\label".len();
                    if let Some((arg, lend)) = parse_braced(inner, lafter) {
                        map.insert(
                            sanitize_label_key(arg.trim()),
                            (format!("({letter})"), String::new()),
                        );
                        edits.push((j + lp, j + lend, String::new()));
                    }
                }
            }
            // Strip subfigure begin/end tags; replace caption with text.
            edits.push((sabs, j, String::new()));
            if let Some((cs, ce)) = cap_span {
                edits.push((cs, ce, replacement.unwrap_or_default()));
            }
            edits.push((send, send_end, String::new()));
            k = send_end;
        }
        i = fend;
    }
    let out = apply_edits(body, &mut edits);
    (out, map)
}

/// Number `table` envs with the SHARED float counter that Typst uses for
/// figures and tables (they interleave in document order). Table bodies stay
/// as Typst tables; only references go static since Typst cannot reference
/// tables directly. Maps `\label{tab:X}` inside each table to ("Table", N).
/// Returns label -> ("Table", number).
pub(crate) fn number_tables(body: &str) -> HashMap<String, (String, String)> {
    if !body.contains("\\begin{table}") {
        return HashMap::new();
    }
    let mut map = HashMap::new();
    let mut count = 0;
    let mut i = 0;
    let b = body.as_bytes();
    while i < b.len() {
        let mut hit: Option<usize> = None; // length of matched begin tag
        let mut is_table = false;
        for env in ["table", "figure"] {
            let tag = format!("\\begin{{{env}}}");
            if body[i..].starts_with(&tag) {
                let after = i + tag.len();
                let next = b.get(after).copied().unwrap_or(b' ');
                // NOTE: the env name already ended at `}`; a following char
                // is content, not a longer name (so `\begin{figure}x`
                // counts). Only the starred form opts out.
                if next == b'*' {
                    break;
                }
                hit = Some(tag.len());
                is_table = env == "table";
                break;
            }
        }
        if let Some(len) = hit {
            count += 1;
            if is_table {
                let num = count.to_string();
                let end_tag = "\\end{table}";
                let inner_end = match body[i + len..].find(end_tag) {
                    Some(p) => i + len + p,
                    None => body.len(),
                };
                let inner = &body[i + len..inner_end];
                let mut k = 0;
                while let Some(lp) = inner[k..].find("\\label") {
                    let labs = k + lp;
                    let lafter = labs + "\\label".len();
                    let lnext = inner.as_bytes().get(lafter).copied().unwrap_or(b' ');
                    if lnext.is_ascii_alphanumeric() || lnext == b'@' {
                        k = lafter;
                        continue;
                    }
                    if let Some((arg, next)) = parse_braced(inner, lafter) {
                        let key = arg.trim().to_string();
                        if key.starts_with("tab:") {
                            map.insert(sanitize_label_key(&key), ("Table".to_string(), num.clone()));
                        }
                        k = next;
                    } else {
                        k = lafter;
                    }
                }
            }
            i += len;
            continue;
        }
        let ch = body[i..].chars().next().unwrap();
        i += ch.len_utf8();
    }
    map
}

/// Move `\label` inside equation/align/gather to right after `\begin` so the
/// label attaches to the equation (mitex emits trailing labels outside math,
/// which Typst cannot reference). Labels in starred envs are dropped.
pub(crate) fn hoist_math_labels(body: &str) -> String {
    let mut out = body.to_string();
    for env in ["equation", "align", "gather"] {
        for starred in [false, true] {
            let tag = if starred {
                format!("\\begin{{{env}*}}")
            } else {
                format!("\\begin{{{env}}}")
            };
            if !out.contains(&tag) {
                continue;
            }
            let end_tag = if starred {
                format!("\\end{{{env}*}}")
            } else {
                format!("\\end{{{env}}}")
            };
            let mut res = String::with_capacity(out.len());
            let mut i = 0;
            let mut dropped_star = 0;
            while let Some(pos) = out[i..].find(&tag) {
                let abs = i + pos;
                res.push_str(&out[i..abs]);
                let after = abs + tag.len();
                let inner_end = match out[after..].find(&end_tag) {
                    Some(p) => after + p,
                    None => {
                        res.push_str(&out[abs..after]);
                        i = after;
                        continue;
                    }
                };
                let inner = out[after..inner_end].to_string();
                // Collect labels.
                let mut keys: Vec<String> = Vec::new();
                let mut cleaned = String::with_capacity(inner.len());
                let mut k = 0;
                while let Some(lp) = inner[k..].find("\\label") {
                    let labs = k + lp;
                    let lafter = labs + "\\label".len();
                    let lnext = inner.as_bytes().get(lafter).copied().unwrap_or(b' ');
                    if lnext.is_ascii_alphanumeric() || lnext == b'@' {
                        cleaned.push_str(&inner[k..lafter]);
                        k = lafter;
                        continue;
                    }
                    match parse_braced(&inner, lafter) {
                        Some((arg, next)) => {
                            keys.push(arg.trim().to_string());
                            cleaned.push_str(&inner[k..labs]);
                            k = next;
                        }
                        None => {
                            cleaned.push_str(&inner[k..lafter]);
                            k = lafter;
                        }
                    }
                }
                cleaned.push_str(&inner[k..]);
                res.push_str(&tag);
                if starred {
                    dropped_star += keys.len();
                } else {
                    for key in &keys {
                        res.push_str(&format!("\\label{{{key}}}"));
                    }
                }
                res.push_str(&cleaned);
                res.push_str(&end_tag);
                i = inner_end + end_tag.len();
            }
            res.push_str(&out[i..]);
            out = res;
            if dropped_star > 0 {
                eprintln!(
                    "warning: dropped {dropped_star} label(s) in unnumbered {env}* (cannot reference)"
                );
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_strip_env() {
        let input = "before \\begin{minipage}[b]{\\linewidth}content\\end{minipage} after";
        assert_eq!(strip_env(input, "minipage"), "before content after");
    }

    #[test]
    fn test_strip_env_options() {
        let input = "\\begin{figure}[htbp]\n\\end{figure}";
        // NOTE: whitespace (including the newline) after the begin tag is
        // consumed along with the options by skip_ws.
        assert_eq!(strip_env_options(input, "figure"), "\\begin{figure}\\end{figure}");
    }

    #[test]
    fn test_rewrite_longtable() {
        let input = "\\begin{longtable}[c]{|l|r|}\nrow\\end{longtable}";
        let res = rewrite_longtable(input);
        assert!(res.starts_with("\\begin{tabular}{ll}"));
        assert!(res.ends_with("\\end{tabular}"));
    }

    #[test]
    fn test_rewrite_tables() {
        let input = "\\begin{tabular*}{\\textwidth}{lcr}\na & b & c\n\\end{tabular*}";
        assert_eq!(rewrite_tables(input), "\\begin{tabular}{lcr}\na & b & c\n\\end{tabular}");
    }

    #[test]
    fn test_hoist_math_labels() {
        let input = "\\begin{equation}x = 1 \\label{eq:test}\\end{equation}";
        assert_eq!(
            hoist_math_labels(input),
            "\\begin{equation}\\label{eq:test}x = 1 \\end{equation}"
        );
    }

    #[test]
    fn test_drop_env_nested_and_unclosed() {
        let nested = "\\begin{tikzpicture}\\begin{tikzpicture}x\\end{tikzpicture}y\\end{tikzpicture}done";
        assert_eq!(drop_env(nested, "tikzpicture"), ("done".to_string(), 1));
        // Unclosed: passed through for mitex to report.
        let unclosed = "a\\begin{tikzpicture}x";
        assert_eq!(drop_env(unclosed, "tikzpicture").1, 0);
        assert!(drop_env(unclosed, "tikzpicture").0.contains("\\begin{tikzpicture}"));
    }

    #[test]
    fn test_extract_algorithms() {
        let body = "t\\begin{algorithm}\\caption{Sort.\\label{alg:s}}\\KwIn{x}\\end{algorithm}See \\ref{alg:s}.";
        let (out, map) = extract_algorithms(body);
        assert_eq!(map.get("alg:s"), Some(&("Algorithm".to_string(), "1".to_string())));
        assert!(out.contains("\\textbf{Algorithm 1.} Sort."), "header: {out}");
        assert!(!out.contains("\\KwIn"), "body dropped: {out}");
        assert!(!out.contains("\\label{alg:s}"), "label dropped: {out}");
    }

    #[test]
    fn test_flatten_subfigures() {
        let body = "\\begin{figure}\\begin{subfigure}{0.5}x\\caption{Left}\\label{fig:l}\\end{subfigure}\\caption{Outer}\\label{fig:o}\\end{figure}";
        let (out, map) = flatten_subfigures(body);
        assert_eq!(map.get("fig:l"), Some(&("(a)".to_string(), String::new())));
        assert!(out.contains("(a) Left"), "caption text: {out}");
        assert!(!out.contains("\\begin{subfigure}"), "tags stripped: {out}");
        assert!(out.contains("\\caption{Outer}"), "outer kept: {out}");
        assert!(out.contains("\\label{fig:o}"), "outer label kept: {out}");
    }

    #[test]
    fn test_rewrite_tables_keep_simple() {
        // Plain l/c/r colspecs are preserved (centering kept).
        let simple = "\\begin{tabular}{cc}\na & b\n\\end{tabular}";
        assert!(rewrite_tables(simple).contains("\\begin{tabular}{cc}"));
        // Anything else (`|`, `@{...}`, `p{...}`) normalizes to plain l's.
        let complex = "\\begin{tabular}{|c|c|}\na & b\n\\end{tabular}";
        assert!(rewrite_tables(complex).contains("\\begin{tabular}{ll}"));
        let wide = "\\begin{tabular}{@{\\extracolsep}m{2cm}l}\na & b\n\\end{tabular}";
        assert!(rewrite_tables(wide).contains("\\begin{tabular}{ll}"));
    }

    #[test]
    fn test_number_tables_shared_counter() {
        let body = "\\begin{table}\\caption{A}\\label{tab:a}\\end{table}\\begin{figure}x\\end{figure}\\begin{table}\\caption{B}\\label{tab:b}\\end{table}";
        let map = number_tables(body);
        // Figures interleave the counter (content may follow the tag directly).
        assert_eq!(map.get("tab:a"), Some(&("Table".to_string(), "1".to_string())));
        assert_eq!(map.get("tab:b"), Some(&("Table".to_string(), "3".to_string())));
    }
}
