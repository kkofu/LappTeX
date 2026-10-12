use std::path::Path;

fn is_escaped(s: &[u8], pos: usize) -> bool {
    let mut n = 0;
    let mut i = pos;
    while i > 0 && s[i - 1] == b'\\' {
        n += 1;
        i -= 1;
    }
    n % 2 == 1
}

/// Strip `%` comments (respecting `\%`). Keeps newlines.
pub(crate) fn strip_comments(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for line in text.split_inclusive('\n') {
        let b = line.as_bytes();
        let mut cut = line.len();
        for (pos, _) in line.bytes().enumerate() {
            if b[pos] == b'%' && !is_escaped(b, pos) {
                cut = pos;
                break;
            }
        }
        out.push_str(&line[..cut]);
        if line.ends_with('\n') {
            out.push('\n');
        }
    }
    out
}

/// Given byte index of `{`, find matching `}`. Returns index of `}`.
pub(crate) fn find_balanced(s: &str, open: usize) -> Option<usize> {
    let b = s.as_bytes();
    if b.get(open) != Some(&b'{') {
        return None;
    }
    let mut depth = 0;
    let mut i = open;
    while i < b.len() {
        if b[i] == b'{' && !is_escaped(b, i) {
            depth += 1;
        } else if b[i] == b'}' && !is_escaped(b, i) {
            depth -= 1;
            if depth == 0 {
                return Some(i);
            }
        }
        i += 1;
    }
    None
}

/// Given byte index of `[`, find matching `]`.
pub(crate) fn find_bracket(s: &str, open: usize) -> Option<usize> {
    let b = s.as_bytes();
    if b.get(open) != Some(&b'[') {
        return None;
    }
    let mut depth = 0;
    let mut i = open;
    while i < b.len() {
        if b[i] == b'[' {
            depth += 1;
        } else if b[i] == b']' {
            depth -= 1;
            if depth == 0 {
                return Some(i);
            }
        }
        i += 1;
    }
    None
}

/// Command/env-name character: ASCII letters and `@` (package internals).
/// Digits are NOT name chars (TeX control-sequence names are letters-only,
/// so `\ref2{x}` is `\ref` followed by text, and `\begin{figure}2` still
/// opens a figure).
pub(crate) fn is_name_char(b: u8) -> bool {
    b.is_ascii_alphabetic() || b == b'@'
}

pub(crate) fn skip_ws(s: &str, mut i: usize) -> usize {
    let b = s.as_bytes();
    while i < b.len() && b[i].is_ascii_whitespace() {
        i += 1;
    }
    i
}

/// Skip zero or more `[...]` bracket groups (skipping whitespace between).
pub(crate) fn skip_brackets(s: &str, mut i: usize) -> usize {
    loop {
        let k = skip_ws(s, i);
        if s.as_bytes().get(k) == Some(&b'[') {
            match find_bracket(s, k) {
                Some(close) => i = close + 1,
                None => break,
            }
        } else {
            return k;
        }
    }
    i
}

/// Skip any sequence of `[...]` and `{...}` groups.
pub(crate) fn skip_bracket_and_braced(s: &str, mut i: usize) -> usize {
    let b = s.as_bytes();
    loop {
        let k = skip_ws(s, i);
        match b.get(k) {
            Some(&b'[') => match find_bracket(s, k) {
                Some(close) => i = close + 1,
                None => break,
            },
            Some(&b'{') => match find_balanced(s, k) {
                Some(close) => i = close + 1,
                None => break,
            },
            _ => break,
        }
    }
    i
}

/// Parse `{arg}` starting at byte index `i` (after skipping whitespace).
/// Returns (content, index_after_closing_brace).
pub(crate) fn parse_braced(s: &str, i: usize) -> Option<(&str, usize)> {
    let i = skip_ws(s, i);
    if s.as_bytes().get(i) != Some(&b'{') {
        return None;
    }
    let close = find_balanced(s, i)?;
    Some((&s[i + 1..close], close + 1))
}

/// If `s[i..]` starts with `\` + one of `names` (exact command name: the next
/// char must not continue the name), returns (index into names, index just
/// after the name). No `\\`-escape check: callers scanning with `find` share
/// these semantics; callers needing it (macros, symbol prefixes) keep their
/// own checks.
pub(crate) fn match_command_at(s: &str, i: usize, names: &[&str]) -> Option<(usize, usize)> {
    if s.as_bytes().get(i) != Some(&b'\\') {
        return None;
    }
    for (idx, name) in names.iter().enumerate() {
        if s[i + 1..].starts_with(name) {
            let after = i + 1 + name.len();
            let next = s.as_bytes().get(after).copied().unwrap_or(b' ');
            if !is_name_char(next) {
                return Some((idx, after));
            }
        }
    }
    None
}

/// Outcome of inspecting one `\name` occurrence in `rewrite_cmd`.
pub(crate) enum CmdEdit {
    /// Leave the occurrence untouched; continue scanning after the name.
    Keep,
    /// Replace `body[start..end]` with text; continue scanning at `end`.
    Replace(String, usize),
}

/// One `\name` occurrence handed to a `rewrite_cmd` callback.
pub(crate) struct CmdHit<'a> {
    pub body: &'a str,
    /// Byte index of the `\`.
    pub start: usize,
    /// Byte index just after the command name.
    pub after: usize,
}

/// Scan non-overlapping `\name` occurrences left to right, letting `f`
/// decide each span. Longer names (`\citep` vs `\cite`) never match.
/// This unifies the dozen `\cmd` scanners that all share the
/// find → boundary-check → parse → rebuild skeleton.
pub(crate) fn rewrite_cmd<F>(body: &str, name: &str, mut f: F) -> String
where
    F: FnMut(CmdHit<'_>) -> CmdEdit,
{
    let needle = format!("\\{name}");
    if !body.contains(&needle) {
        return body.to_string();
    }
    let mut out = String::with_capacity(body.len());
    let mut i = 0;
    while let Some(pos) = body[i..].find(&needle) {
        let abs = i + pos;
        let after = match match_command_at(body, abs, &[name]) {
            Some((_, after)) => after,
            None => {
                out.push_str(&body[i..abs + needle.len()]);
                i = abs + needle.len();
                continue;
            }
        };
        match f(CmdHit {
            body,
            start: abs,
            after,
        }) {
            CmdEdit::Keep => {
                out.push_str(&body[i..after]);
                i = after;
            }
            CmdEdit::Replace(text, end) => {
                out.push_str(&body[i..abs]);
                out.push_str(&text);
                i = end;
            }
        }
    }
    out.push_str(&body[i..]);
    out
}

/// Skip whitespace, one `*`, and any `[...]` groups; returns the new index.
/// (The common `\cmd*[...][...]` prefix shape before `{args}`.)
/// NOTE: consumes any number of bracket groups; three or more on one command
/// do not occur in TeX, so this is equivalent to the old 1–2 group versions.
pub(crate) fn skip_star_brackets(s: &str, i: usize) -> usize {
    let mut j = skip_ws(s, i);
    if s.as_bytes().get(j) == Some(&b'*') {
        j = skip_ws(s, j + 1);
    }
    loop {
        let k = skip_ws(s, j);
        if s.as_bytes().get(k) == Some(&b'[') {
            match find_bracket(s, k) {
                Some(close) => j = close + 1,
                None => break,
            }
        } else {
            j = k;
            break;
        }
    }
    j
}

pub(crate) fn read_file_lossy(p: &Path) -> Result<String, String> {
    let b = std::fs::read(p).map_err(|e| format!("Failed to read {}: {e}", p.display()))?;
    Ok(String::from_utf8_lossy(&b).into_owned())
}

/// Apply sorted span edits `(start, end, replacement)` to body.
/// Overlapping edits are skipped (first wins).
pub(crate) fn apply_edits(body: &str, edits: &mut [(usize, usize, String)]) -> String {
    edits.sort_by_key(|e| e.0);
    let mut out = String::with_capacity(body.len());
    let mut pos = 0;
    for (s, e, r) in edits.iter() {
        if *s < pos {
            continue; // overlapping (shouldn't happen)
        }
        out.push_str(&body[pos..*s]);
        out.push_str(r);
        pos = *e;
    }
    out.push_str(&body[pos..]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_strip_comments() {
        let input = "hello % world\nfoo \\% bar % comment\nbaz";
        let expected = "hello \nfoo \\% bar \nbaz";
        assert_eq!(strip_comments(input), expected);
    }

    #[test]
    fn test_find_balanced() {
        assert_eq!(find_balanced("{hello {world}}", 0), Some(14));
        assert_eq!(find_balanced("{\\{not close}}", 0), Some(12));
        assert_eq!(find_balanced("{unclosed", 0), None);
        assert_eq!(find_balanced("not brace", 0), None);
    }

    #[test]
    fn test_find_bracket() {
        assert_eq!(find_bracket("[opt[nested]]", 0), Some(12));
        assert_eq!(find_bracket("[unclosed", 0), None);
    }

    #[test]
    fn test_parse_braced() {
        assert_eq!(parse_braced("  {content}rest", 0), Some(("content", 11)));
        assert_eq!(parse_braced("no brace", 0), None);
    }

    #[test]
    fn test_skip_helpers() {
        assert_eq!(skip_brackets("  [a][b] x", 0), 9);
        assert_eq!(skip_brackets("  x", 0), 2);
        assert_eq!(skip_bracket_and_braced(" [a] {b} x", 0), 8);
        assert_eq!(skip_star_brackets(" *[a] {b}", 0), 6);
        assert!(is_name_char(b'a'));
        assert!(is_name_char(b'@'));
        assert!(!is_name_char(b' '));
        // Digits are NOT name chars (TeX names are letters-only).
        assert!(!is_name_char(b'2'));
    }

    #[test]
    fn test_match_command_at() {
        // Hit, longer-name miss, non-command position.
        assert_eq!(match_command_at("\\cite{x}", 0, &["cite"]), Some((0, 5)));
        assert_eq!(match_command_at("\\citet{x}", 0, &["cite"]), None);
        assert_eq!(match_command_at("x\\cite", 1, &["cite"]), Some((0, 6)));
        assert_eq!(match_command_at("plain", 0, &["cite"]), None);
        // First boundary-ok name wins.
        assert_eq!(
            match_command_at("\\ref{x}", 0, &["Cref", "ref"]),
            Some((1, 4))
        );
        // A digit ends the name (does not extend it).
        assert_eq!(match_command_at("\\ref2{x}", 0, &["ref"]), Some((0, 4)));
    }
}
