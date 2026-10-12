use crate::tex::{
    find_balanced, find_bracket, is_name_char, parse_braced, rewrite_cmd, skip_brackets,
    skip_star_brackets, skip_ws, CmdEdit,
};

/// Replace `\old{X}` (and `\old*{X}`) with `\new{X}`.
pub(crate) fn rename_command_with_arg(text: &str, old: &str, new: &str) -> String {
    rewrite_cmd(text, old, |h| {
        let mut j = h.after;
        if h.body.as_bytes().get(j) == Some(&b'*') {
            j += 1; // drop the star
        }
        match parse_braced(h.body, j) {
            Some((arg, next_i)) => CmdEdit::Replace(format!("\\{new}{{{arg}}}"), next_i),
            None => CmdEdit::Keep,
        }
    })
}

/// Remove characters that are illegal in Typst labels from the arguments of
/// \label, \ref, \Cref, \cref, \autoref, \Autoref (applied uniformly so that
/// label definitions and references stay consistent).
pub(crate) fn sanitize_label_args(body: &str) -> String {
    let mut out = body.to_string();
    for name in ["label", "ref", "Cref", "cref", "autoref", "Autoref"] {
        out = rewrite_cmd(&out, name, |h| {
            let j = skip_star_brackets(h.body, h.after);
            match parse_braced(h.body, j) {
                Some((arg, next_i)) => {
                    let clean: String = arg
                        .split(',')
                        .map(|k| sanitize_label_key(k.trim()))
                        .collect::<Vec<_>>()
                        .join(",");
                    // Re-emit: \name + (star/brackets span verbatim) + {clean}.
                    CmdEdit::Replace(
                        format!("\\{name}{}{{{clean}}}", &h.body[h.after..j]),
                        next_i,
                    )
                }
                None => CmdEdit::Keep,
            }
        });
    }
    out
}

/// Sanitize one label key for Typst: drop quotes, map anything outside
/// `[A-Za-z0-9_:.\-]` to `-` (applied uniformly to defs and refs).
/// Runs of `-` are collapsed: mitex converts `--` to an en-dash ligature on
/// the definition side but not inside ref arguments, so both sides must agree.
pub(crate) fn sanitize_label_key(k: &str) -> String {
    let mut out = String::with_capacity(k.len());
    let mut prev_dash = false;
    for ch in k.chars() {
        if ch.is_ascii_alphanumeric() || matches!(ch, ':' | '_' | '.') {
            out.push(ch);
            prev_dash = false;
        } else if !matches!(ch, '\'' | '’' | '‘' | '`') {
            if !prev_dash {
                out.push('-');
                prev_dash = true;
            }
        }
    }
    out
}

/// `\cite{a,b}` -> `\cite{a}, \cite{b}` (mitex/typst cannot handle multi-key
/// cites: the whole key list would be looked up as one key; prenotes also
/// confuse mitex, so `[...]` notes are dropped).
pub(crate) fn split_multicite(body: &str) -> String {
    rewrite_cmd(body, "cite", |h| {
        let j = skip_star_brackets(h.body, h.after);
        match parse_braced(h.body, j) {
            Some((arg, next_i)) => {
                let keys: Vec<&str> =
                    arg.split(',').map(|k| k.trim()).filter(|k| !k.is_empty()).collect();
                if keys.is_empty() {
                    CmdEdit::Replace(h.body[h.start..next_i].to_string(), next_i)
                } else {
                    CmdEdit::Replace(
                        keys.iter()
                            .map(|k| format!("\\cite{{{k}}}"))
                            .collect::<Vec<_>>()
                            .join(", "),
                        next_i,
                    )
                }
            }
            None => CmdEdit::Keep,
        }
    })
}

/// Replace `\name[...]{a}...{z}` with the content of the LAST braced group.
/// Used for `\multirow{n}{w}{text}`, `\multicolumn{n}{spec}{text}`,
/// `\raisebox{a}{b}`, `\resizebox{a}{b}{c}`, `\textcolor{c}{t}`, `\uline{t}`.
/// If no braced group follows, the text is left untouched.
pub(crate) fn keep_last_arg(body: &str, name: &str) -> String {
    rewrite_cmd(body, name, |h| {
        let mut j = skip_ws(h.body, h.after);
        let mut last: Option<(usize, usize)> = None;
        let mut end = j;
        loop {
            let k = skip_ws(h.body, j);
            match h.body.as_bytes().get(k) {
                Some(&b'[') => match find_bracket(h.body, k) {
                    Some(close) => j = close + 1,
                    None => break,
                },
                Some(&b'{') => match find_balanced(h.body, k) {
                    Some(close) => {
                        last = Some((k + 1, close));
                        j = close + 1;
                    }
                    None => break,
                },
                _ => {
                    end = j;
                    break;
                }
            }
        }
        match last {
            Some((s, e)) => CmdEdit::Replace(h.body[s..e].to_string(), end),
            None => CmdEdit::Keep,
        }
    })
}

/// Replace `\old` with `\new` (no argument handling) for commands that take
/// no braced arg, e.g. sized delimiters `\Bigl(` -> `\Big(`.
pub(crate) fn rename_command_bare(body: &str, old: &str, new: &str) -> String {
    let replacement = format!("\\{new}");
    rewrite_cmd(body, old, |h| {
        CmdEdit::Replace(replacement.clone(), h.after)
    })
}

fn starts_with_any_ref(s: &str, labs: &[&str]) -> bool {
    ["\\ref{", "\\Cref{", "\\cref{"].iter().any(|cmd| {
        s.starts_with(cmd) && labs.iter().any(|lab| s[cmd.len()..].starts_with(lab))
    })
}

/// Drop manual `Fig.`/`Figure`/`section` prefixes before references
/// (`Fig.~\ref{fig:x}`, `(section~\ref{sec:x})`): Typst renders native refs
/// as "Figure N" / "Section N", so keeping the prefix would duplicate it.
pub(crate) fn strip_ref_prefix(body: &str) -> String {
    let out = strip_word_prefix(body);
    strip_symbol_prefix(&out, "\\S", &["sec:", "app:"])
}

fn strip_word_prefix(body: &str) -> String {
    let mut out = body.to_string();
    for (prefix, labs) in [
        ("Fig.", &["fig:"][..]),
        ("Figure", &["fig:"][..]),
        ("section", &["sec:", "app:"][..]),
        ("Section", &["sec:", "app:"][..]),
    ] {
        if !out.contains(prefix) {
            continue;
        }
        let mut res = String::with_capacity(out.len());
        let mut i = 0;
        while let Some(pos) = out[i..].find(prefix) {
            let abs = i + pos;
            let prev_ok = abs == 0 || !out.as_bytes()[abs - 1].is_ascii_alphanumeric();
            let after = abs + prefix.len();
            let mut j = skip_ws(&out, after);
            if out.as_bytes().get(j) == Some(&b'~') {
                j = skip_ws(&out, j + 1);
            }
            if prev_ok && starts_with_any_ref(&out[j..], labs) {
                res.push_str(&out[i..abs]);
                i = j;
            } else {
                res.push_str(&out[i..after]);
                i = after;
            }
        }
        res.push_str(&out[i..]);
        out = res;
    }
    out
}

/// Drop `\S` (section symbol) before section references: mitex expands it to
/// the word "section", duplicating Typst's native "Section N" supplement.
fn strip_symbol_prefix(body: &str, sym: &str, labs: &[&str]) -> String {
    if !body.contains(sym) {
        return body.to_string();
    }
    let mut res = String::with_capacity(body.len());
    let mut i = 0;
    while let Some(pos) = body[i..].find(sym) {
        let abs = i + pos;
        let after = abs + sym.len();
        let next = body.as_bytes().get(after).copied().unwrap_or(b' ');
        if is_name_char(next) || (abs > 0 && body.as_bytes()[abs - 1] == b'\\') {
            res.push_str(&body[i..after]);
            i = after;
            continue;
        }
        let mut j = skip_ws(body, after);
        if body.as_bytes().get(j) == Some(&b'~') {
            j = skip_ws(body, j + 1);
        }
        if starts_with_any_ref(&body[j..], labs) {
            res.push_str(&body[i..abs]);
            i = j;
        } else {
            res.push_str(&body[i..after]);
            i = after;
        }
    }
    res.push_str(&body[i..]);
    res
}

/// `\Cref{a,b}` etc -> `\ref{a}, \ref{b}`.
pub(crate) fn rewrite_ref_like(body: &str, name: &str) -> String {
    rewrite_cmd(body, name, |h| {
        let mut j = h.after;
        if h.body.as_bytes().get(j) == Some(&b'*') {
            j += 1;
        }
        let j = skip_brackets(h.body, j);
        match parse_braced(h.body, j) {
            Some((arg, next_i)) => {
                let refs = arg
                    .split(',')
                    .map(|k| format!("\\ref{{{}}}", k.trim()))
                    .collect::<Vec<_>>()
                    .join(", ");
                CmdEdit::Replace(refs, next_i)
            }
            None => CmdEdit::Keep,
        }
    })
}

/// `\texorpdfstring{TEX}{PDF}` -> `TEX`.
pub(crate) fn texorpdfstring_to_first(body: &str) -> String {
    rewrite_cmd(body, "texorpdfstring", |h| {
        let (a, j) = match parse_braced(h.body, h.after) {
            Some(v) => v,
            None => return CmdEdit::Keep,
        };
        // Skip the second arg.
        let mut k = j;
        if h.body.as_bytes().get(k) == Some(&b'{') {
            if let Some(close) = find_balanced(h.body, k) {
                k = close + 1;
            }
        }
        CmdEdit::Replace(a.to_string(), k)
    })
}

pub(crate) fn rewrite_crefrange(body: &str) -> String {
    rewrite_cmd(body, "Crefrange", |h| {
        let (a, j) = match parse_braced(h.body, h.after) {
            Some(v) => v,
            None => return CmdEdit::Keep,
        };
        match parse_braced(h.body, j) {
            Some((b2, next_i)) => CmdEdit::Replace(
                format!("\\ref{{{}}}--\\ref{{{}}}", a.trim(), b2.trim()),
                next_i,
            ),
            None => CmdEdit::Keep,
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sanitize_label_key() {
        assert_eq!(sanitize_label_key("sec:intro_1"), "sec:intro_1");
        assert_eq!(sanitize_label_key("foo's--bar"), "foos-bar");
        assert_eq!(sanitize_label_key("a---b...c"), "a-b...c");
    }

    #[test]
    fn test_sanitize_label_args() {
        let input = "See \\ref{fig:foo'1, bar--2} and \\label{sec:a'b}.";
        let expected = "See \\ref{fig:foo1,bar-2} and \\label{sec:ab}.";
        assert_eq!(sanitize_label_args(input), expected);
    }

    #[test]
    fn test_split_multicite() {
        let input = "\\cite{a, b, c} and \\cite[p. 5]{x}";
        let expected = "\\cite{a}, \\cite{b}, \\cite{c} and \\cite{x}";
        assert_eq!(split_multicite(input), expected);
    }

    #[test]
    fn test_keep_last_arg() {
        assert_eq!(keep_last_arg("\\multirow{2}{*}{\\textbf{hi}}", "multirow"), "\\textbf{hi}");
        assert_eq!(keep_last_arg("\\textcolor{red}{text}", "textcolor"), "text");
    }

    #[test]
    fn test_strip_ref_prefix() {
        assert_eq!(strip_ref_prefix("Figure~\\ref{fig:x}"), "\\ref{fig:x}");
        assert_eq!(strip_ref_prefix("Section \\ref{sec:x}"), "\\ref{sec:x}");
        assert_eq!(strip_ref_prefix("\\S\\ref{sec:x}"), "\\ref{sec:x}");
    }

    #[test]
    fn test_rewrite_ref_like() {
        assert_eq!(rewrite_ref_like("\\Cref{a, b}", "Cref"), "\\ref{a}, \\ref{b}");
    }

    #[test]
    fn test_rewrite_crefrange() {
        assert_eq!(rewrite_crefrange("\\Crefrange{sec:1}{sec:3}"), "\\ref{sec:1}--\\ref{sec:3}");
    }

    #[test]
    fn test_texorpdfstring() {
        assert_eq!(texorpdfstring_to_first("\\texorpdfstring{TeX}{PDF}"), "TeX");
    }

    #[test]
    fn test_rename_bare_boundary() {
        assert_eq!(rename_command_bare("\\Bigl(x\\Bigr)", "Bigl", "Big"), "\\Big(x\\Bigr)");
        // Longer names are untouched.
        assert_eq!(rename_command_bare("\\Biglx", "Bigl", "Big"), "\\Biglx");
    }

    #[test]
    fn test_strip_prefix_variants() {
        // Section/app prefixes also strip.
        assert_eq!(strip_ref_prefix("(section~\\ref{sec:x})"), "(\\ref{sec:x})");
        assert_eq!(strip_ref_prefix("\\S\\ref{app:y}"), "\\ref{app:y}");
        // `\\S` (linebreak + S) is not a command.
        assert_eq!(strip_ref_prefix("a\\\\S\\ref{sec:x}"), "a\\\\S\\ref{sec:x}");
        // Non-ref following text is kept.
        assert_eq!(strip_ref_prefix("Figure 1 shows"), "Figure 1 shows");
    }

    #[test]
    fn test_keep_last_arg_no_groups() {
        // No braced group: untouched.
        assert_eq!(keep_last_arg("a \\uline b", "uline"), "a \\uline b");
    }
}
