use crate::preprocess::has_command;
use crate::tex::{is_name_char, parse_braced, rewrite_cmd, skip_brackets, skip_ws, CmdEdit};

/// Resolve references to `\paragraph` headings as static italic text, since
/// paragraphs demote to bold run-ins that Typst cannot reference.
/// Finds `\paragraph{T}` followed (within a few lines) by `\label{L}`,
/// maps L -> T, replaces `\ref{L}` with `\textit{T}`, and drops the label
/// definitions. Must run on text with `\paragraph` still intact.
pub(crate) fn resolve_para_refs(body: &str) -> String {
    // Collect (label -> title).
    let mut map: Vec<(String, String)> = Vec::new();
    let b = body.as_bytes();
    let mut i = 0;
    while let Some(pos) = body[i..].find("\\paragraph") {
        let abs = i + pos;
        let after = abs + "\\paragraph".len();
        let next = b.get(after).copied().unwrap_or(b' ');
        if is_name_char(next) {
            i = after;
            continue;
        }
        let mut j = skip_ws(body, after);
        if b.get(j) == Some(&b'*') {
            j = skip_ws(body, j + 1);
        }
        let (title, mut k) = match parse_braced(body, j) {
            Some((arg, next)) => (arg.trim().to_string(), next),
            None => {
                i = after;
                continue;
            }
        };
        // Allow up to 3 newlines before the label.
        let mut newlines = 0;
        while k < b.len() && b[k].is_ascii_whitespace() {
            if b[k] == b'\n' {
                newlines += 1;
            }
            k += 1;
        }
        if newlines <= 3 && body[k..].starts_with("\\label") {
            let lafter = k + "\\label".len();
            if let Some((arg, _)) = parse_braced(body, lafter) {
                let key = arg.trim().to_string();
                if !key.is_empty() {
                    map.push((key, title));
                }
            }
        }
        i = j;
    }
    if map.is_empty() {
        return body.to_string();
    }
    let mut out = body.to_string();
    for (key, title) in &map {
        // Drop the label definition.
        let lab = format!("\\label{{{key}}}");
        out = out.replace(&lab, "");
        // Static-ize references: any ref-form -> \textit{T}.
        let title_fmt = format!("\\textit{{{title}}}");
        for cmd in ["\\ref", "\\Cref", "\\cref", "\\autoref", "\\Autoref", "\\eqref"] {
            for form in [
                format!("{cmd}{{{key}}}"),
                format!("{cmd}*{{{key}}}"),
            ] {
                out = out.replace(&form, &title_fmt);
            }
            out = strip_ref_notes(&out, cmd, key, title);
        }
    }
    out
}

/// Replace `\cmd[...]{key}` (with notes) by static italic title.
fn strip_ref_notes(body: &str, cmd: &str, key: &str, title: &str) -> String {
    // `cmd` arrives with a leading backslash; strip it for the name match.
    let name = cmd.trim_start_matches('\\');
    rewrite_cmd(body, name, |h| {
        let mut j = skip_ws(h.body, h.after);
        if h.body.as_bytes().get(j) == Some(&b'*') {
            j = skip_ws(h.body, j + 1);
        }
        // Only notes-forms (`\cmd[..]{key}`) are rewritten.
        if h.body.as_bytes().get(j) != Some(&b'[') {
            return CmdEdit::Keep;
        }
        let j = skip_brackets(h.body, j);
        match parse_braced(h.body, j) {
            Some((arg, next)) if arg.trim() == key => {
                CmdEdit::Replace(format!("\\textit{{{title}}}"), next)
            }
            _ => CmdEdit::Keep,
        }
    })
}

/// Correct single-pass heading demotion (call on original body).
/// Returns (rewritten_body, unnumbered_headings as (level, text), has_chapters).
pub(crate) fn demote_headings(body: &str) -> (String, Vec<(u8, String)>, bool) {
    let mut out = String::with_capacity(body.len());
    let mut stars = Vec::new();
    let mut i = 0;
    let b = body.as_bytes();
    // If the document has chapters, demote everything one level (chapter->L1).
    // Otherwise (article-style) keep section levels as-is.
    let has_chapters = has_command(body, "chapter") || has_command(body, "part");
    let table: &[(&str, &str)] = if has_chapters {
        &[
            ("chapter", "section"),
            ("part", "section"),
            ("section", "subsection"),
            ("subsection", "subsubsection"),
            ("subsubsection", "textbf"),
        ]
    } else {
        &[
            ("section", "section"),
            ("subsection", "subsection"),
            ("subsubsection", "subsubsection"),
            ("paragraph", "subsubsection"),
        ]
    };
    while i < b.len() {
        if b[i] == b'\\' {
            let mut matched: Option<(&str, &str, usize)> = None;
            for (old, new) in table {
                let needle = format!("\\{old}");
                if body[i..].starts_with(&needle) {
                    let after = i + needle.len();
                    let next = b.get(after).copied().unwrap_or(b' ');
                    if !is_name_char(next) {
                        matched = Some((*old, *new, after));
                        break;
                    }
                }
            }
            if let Some((old, new, after)) = matched {
                let mut j = after;
                let starred = b.get(j) == Some(&b'*');
                if starred {
                    j += 1;
                }
                if let Some((arg, next_i)) = parse_braced(body, j) {
                    if starred {
                        stars.push((level_of(new), arg.to_string()));
                    } else if !has_chapters && old == "paragraph" {
                        // \paragraph demoted to a heading: render unnumbered.
                        stars.push((3, unescape_tex(arg)));
                    }
                    out.push_str(&format!("\\{new}{{{arg}}}"));
                    i = next_i;
                    continue;
                }
            }
        }
        let ch = body[i..].chars().next().unwrap();
        out.push(ch);
        i += ch.len_utf8();
    }
    (out, stars, has_chapters)
}

fn level_of(new: &str) -> u8 {
    match new {
        "section" => 1,
        "subsection" => 2,
        _ => 3,
    }
}

/// Unescape common TeX escapes for matching rendered heading text
/// (`\&` renders as `&` in the .typ output).
fn unescape_tex(s: &str) -> String {
    let mut out = s.to_string();
    for (from, to) in [
        ("\\&", "&"),
        ("\\%", "%"),
        ("\\_", "_"),
        ("\\#", "#"),
        ("\\$", "$"),
        ("\\{", "{"),
        ("\\}", "}"),
    ] {
        out = out.replace(from, to);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_demote_headings_article() {
        let input = "\\section{Intro}\n\\paragraph{Detail}";
        let (out, stars, has_chap) = demote_headings(input);
        assert!(!has_chap);
        assert_eq!(out, "\\section{Intro}\n\\subsubsection{Detail}");
        assert_eq!(stars, vec![(3, "Detail".to_string())]);
    }

    #[test]
    fn test_demote_headings_book() {
        let input = "\\chapter{First}\n\\section{Intro}";
        let (out, _, has_chap) = demote_headings(input);
        assert!(has_chap);
        assert_eq!(out, "\\section{First}\n\\subsection{Intro}");
    }

    #[test]
    fn test_resolve_para_refs() {
        let input = "\\paragraph{Foo}\n\\label{p:foo}\nIn \\ref{p:foo}.";
        let out = resolve_para_refs(input);
        assert_eq!(out, "\\paragraph{Foo}\n\nIn \\textit{Foo}.");
    }

    #[test]
    fn test_resolve_para_refs_all_forms() {
        // Cref/autoref/eqref forms and bracket notes all go static.
        let input = "\\paragraph{Bar}\n\\label{p:bar}\n\\Cref{p:bar}, \\autoref{p:bar}, \\eqref{p:bar}, \\Cref[n]{p:bar}.";
        let out = resolve_para_refs(input);
        assert!(!out.contains("\\Cref") && !out.contains("\\autoref") && !out.contains("\\eqref"), "all static: {out}");
        assert_eq!(out.matches("\\textit{Bar}").count(), 4);
    }

    #[test]
    fn test_demote_records_paragraph_unnumbered() {
        // Article-style: paragraph -> subsubsection + unnumbered record.
        let (out, stars, _) = demote_headings("\\paragraph{Detail}");
        assert_eq!(out, "\\subsubsection{Detail}");
        assert_eq!(stars, vec![(3u8, "Detail".to_string())]);
    }
}
