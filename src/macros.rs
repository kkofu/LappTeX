use crate::tex::{is_name_char, parse_braced, skip_ws};

/// A `\newcommand`/`\renewcommand`/`\def` macro definition.
pub(crate) struct MacroDef {
    pub(crate) name: String,
    pub(crate) nargs: usize,
    pub(crate) body: String,
}

/// Collect macro definitions (`\newcommand`, `\renewcommand`,
/// `\providecommand`, `\def`) from text, removing their occurrences.
/// Returns (text_without_defs, defs). First definition wins on duplicates.
pub(crate) fn extract_macros(text: &str) -> (String, Vec<MacroDef>) {
    let mut defs: Vec<MacroDef> = Vec::new();
    let mut spans: Vec<(usize, usize)> = Vec::new();
    let mut i = 0;
    let b = text.as_bytes();
    while i < b.len() {
        let mut matched = false;
        for cmd in ["\\newcommand", "\\renewcommand", "\\providecommand"] {
            if text[i..].starts_with(cmd) {
                let after = i + cmd.len();
                let next = b.get(after).copied().unwrap_or(b' ');
                if is_name_char(next) {
                    break;
                }
                let mut j = skip_ws(text, after);
                if b.get(j) == Some(&b'*') {
                    j = skip_ws(text, j + 1);
                }
                // Name: {\name} or \name
                let (name, k) = if b.get(j) == Some(&b'{') {
                    match parse_braced(text, j) {
                        Some((arg, next)) => (arg.trim().trim_start_matches('\\').to_string(), next),
                        None => continue,
                    }
                } else if b.get(j) == Some(&b'\\') {
                    let mut m = j + 1;
                    while m < b.len() && is_name_char(b[m]) {
                        m += 1;
                    }
                    if m == j + 1 {
                        continue;
                    }
                    (text[j + 1..m].to_string(), m)
                } else {
                    continue;
                };
                let mut l = skip_ws(text, k);
                // Optional [nargs]
                let mut nargs = 0;
                if b.get(l) == Some(&b'[') {
                    // Find closing ] on the same line-ish (simple scan for digits).
                    let mut m = l + 1;
                    while m < b.len() && b[m].is_ascii_digit() {
                        m += 1;
                    }
                    if b.get(m) == Some(&b']') && m > l + 1 {
                        nargs = text[l + 1..m].parse().unwrap_or(0);
                        l = skip_ws(text, m + 1);
                    } else {
                        // Possibly [default] optional-arg form: skip balanced [...] then {n}?
                        // Unsupported: leave the whole definition in place.
                        continue;
                    }
                }
                if let Some((arg, next)) = parse_braced(text, l) {
                    matched = true;
                    // stash body via span end; body = arg
                    spans.push((i, next));
                    defs.push(MacroDef {
                        name,
                        nargs,
                        body: arg.to_string(),
                    });
                    i = next;
                    break;
                }
            }
        }
        if matched {
            continue;
        }
        if text[i..].starts_with("\\def") {
            let after = i + "\\def".len();
            let next = b.get(after).copied().unwrap_or(b' ');
            if is_name_char(next) {
                i += 1;
                continue;
            }
            let j = skip_ws(text, after);
            if b.get(j) == Some(&b'\\') {
                let mut m = j + 1;
                while m < b.len() && is_name_char(b[m]) {
                    m += 1;
                }
                let name = text[j + 1..m].to_string();
                // Consecutive #<digit> parameter tokens.
                let mut nargs = 0;
                let mut k = skip_ws(text, m);
                while b.get(k) == Some(&b'#')
                    && b.get(k + 1).map(|c| c.is_ascii_digit()).unwrap_or(false)
                {
                    nargs = nargs.max((b[k + 1] - b'0') as usize);
                    k = skip_ws(text, k + 2);
                }
                if let Some((arg, next)) = parse_braced(text, k) {
                    spans.push((i, next));
                    defs.push(MacroDef {
                        name,
                        nargs,
                        body: arg.to_string(),
                    });
                    i = next;
                    continue;
                }
            }
        }
        let ch = text[i..].chars().next().unwrap();
        i += ch.len_utf8();
    }
    // Deduplicate (first wins), rebuild text without def spans.
    let mut seen = std::collections::HashSet::new();
    let mut unique = Vec::new();
    for d in defs {
        if seen.insert(d.name.clone()) {
            unique.push(d);
        }
    }
    let mut out = String::with_capacity(text.len());
    let mut pos = 0;
    let mut sorted = spans;
    sorted.sort();
    for (s, e) in sorted {
        if s < pos {
            continue;
        }
        out.push_str(&text[pos..s]);
        pos = e;
    }
    out.push_str(&text[pos..]);
    (out, unique)
}

/// Expand macro call sites textually (fixpoint, bounded rounds).
pub(crate) fn expand_macros(text: &str, defs: &[MacroDef]) -> String {
    let mut out = text.to_string();
    for _ in 0..20 {
        let mut changed = false;
        for d in defs {
            let (next, c) = expand_one(&out, d);
            out = next;
            changed |= c;
        }
        if !changed {
            break;
        }
    }
    out
}

fn expand_one(text: &str, def: &MacroDef) -> (String, bool) {
    let needle = format!("\\{}", def.name);
    let b = text.as_bytes();
    // Quick check to avoid rebuilding when absent.
    if !text.contains(&needle) {
        return (text.to_string(), false);
    }
    let mut res = String::with_capacity(text.len());
    let mut i = 0;
    let mut changed = false;
    while let Some(pos) = text[i..].find(&needle) {
        let abs = i + pos;
        // Not `\\name` text.
        if abs > 0 && b[abs - 1] == b'\\' {
            res.push_str(&text[i..abs + 1]);
            i = abs + 1;
            continue;
        }
        let after = abs + needle.len();
        let next = b.get(after).copied().unwrap_or(b' ');
        if is_name_char(next) {
            // Longer macro name (e.g. \kimi vs \kimilogo): skip.
            res.push_str(&text[i..after]);
            i = after;
            continue;
        }
        // Parse up to nargs braced args.
        let mut j = skip_ws(text, after);
        let mut args: Vec<&str> = Vec::new();
        let mut ok = true;
        for _ in 0..def.nargs {
            match parse_braced(text, j) {
                Some((arg, next)) => {
                    args.push(arg);
                    j = next;
                }
                None => {
                    ok = false;
                    break;
                }
            }
        }
        if !ok {
            res.push_str(&text[i..after]);
            i = after;
            continue;
        }
        let mut body = def.body.clone();
        for (n, a) in args.iter().enumerate() {
            body = body.replace(&format!("#{}", n + 1), a);
        }
        res.push_str(&text[i..abs]);
        res.push_str(&body);
        i = j;
        changed = true;
    }
    res.push_str(&text[i..]);
    (res, changed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_and_expand_macros() {
        let input = "\\newcommand{\\myword}{Universe}\n\\newcommand{\\greet}[1]{Hello #1!}\n\\greet{\\myword}";
        let (body, defs) = extract_macros(input);
        assert_eq!(defs.len(), 2);
        let expanded = expand_macros(&body, &defs);
        assert_eq!(expanded.trim(), "Hello Universe!");
    }

    #[test]
    fn test_expand_exact_name_only() {
        // \kimi must not fire inside \kimilogo.
        let (body, defs) = extract_macros("\\newcommand{\\kimi}[1]{Kimi K#1}\n\\kimilogo and \\kimi{3}");
        assert_eq!(defs.len(), 1);
        let out = expand_macros(&body, &defs);
        assert!(out.contains("\\kimilogo"), "longer name kept: {out}");
        assert!(out.contains("Kimi K3"), "call expanded: {out}");
    }

    #[test]
    fn test_expand_def_params_and_noarg() {
        let (body, defs) = extract_macros("\\def\\hbw{0.92}\n\\def\\twice#1#2{#1#2}\nw=\\hbw, \\twice{A}{B}");
        assert_eq!(defs.len(), 2);
        let out = expand_macros(&body, &defs);
        assert!(out.contains("w=0.92"), "def expanded: {out}");
        assert!(out.contains("AB"), "params expanded: {out}");
        assert!(!out.contains("\\newcommand") && !out.contains("\\def\\hbw"), "defs removed");
    }
}
