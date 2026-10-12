use std::collections::HashSet;

use crate::cmd::warn;
use crate::preprocess::Processed;

// ---------------------------------------------------------------------------
// .typ post-processing
// ---------------------------------------------------------------------------

fn typst_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '[' | ']' | '#' | '$' | '@' | '<' | '>' | '*' | '_' | '`' => {
                out.push('\\');
                out.push(ch);
            }
            '~' => out.push(' '),
            _ => out.push(ch),
        }
    }
    out
}

// ---------------------------------------------------------------------------
// .typ prelude pruning
// ---------------------------------------------------------------------------

/// mitex-cli emits `#let NAME = mitex-scope.at("NAME", default: none);` for every
/// known alias. For aliases that have NO entry in mitex-scope (typically names
/// that coincide with native Typst functions like `emph`, `figure`, `image`),
/// this shadows the native definition with `none` and every call fails with
/// "expected function, found none". Query the real scope keys in-process and
/// drop the shadowing lines so native definitions shine through.
fn query_scope_keys(import_spec: &str) -> Result<HashSet<String>, String> {
    crate::typstlib::query_mitex_scope(import_spec)
}

pub(crate) fn mitex_import_spec(typ: &str) -> Option<String> {
    // `#import "@preview/mitex:0.2.7": *`
    let pos = typ.find("#import \"@preview/mitex:")?;
    let rest = &typ[pos + "#import \"".len()..];
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}

/// Drop `#let NAME = mitex-scope.at(...)` lines for NAMEs not in `keys`.
/// Returns (pruned_text, dropped_names).
fn prune_prelude(typ: &str, keys: &HashSet<String>) -> (String, Vec<String>) {
    let mut out = String::with_capacity(typ.len());
    let mut dropped = Vec::new();
    for line in typ.lines() {
        let t = line.trim_start();
        if let Some(rest) = t.strip_prefix("#let ") {
            if let Some(eq) = rest.find('=') {
                let name = rest[..eq].trim();
                let rhs = rest[eq + 1..].trim_start();
                if rhs.starts_with("mitex-scope.at(\"") && !keys.contains(name) {
                    dropped.push(name.to_string());
                    continue;
                }
            }
        }
        out.push_str(line);
        out.push('\n');
    }
    (out, dropped)
}

/// Query mitex-scope keys and drop shadowing prelude lines.
/// Returns (pruned_text, scope_keys). On query failure, warns and returns the
/// text unchanged with an empty key set.
pub(crate) fn prune_shadowed_prelude(
    typ: &str,
    spec: &str,
) -> Result<(String, HashSet<String>), String> {
    match query_scope_keys(spec) {
        Ok(keys) => {
            let (pruned, dropped) = prune_prelude(typ, &keys);
            if !dropped.is_empty() {
                eprintln!(
                    "pruned {} shadowed prelude binding(s) missing from mitex-scope",
                    dropped.len()
                );
            }
            Ok((pruned, keys))
        }
        Err(e) => {
            warn(&format!("Could not query mitex-scope ({e}); keeping prelude as-is"));
            Ok((typ.to_string(), HashSet::new()))
        }
    }
}

/// Fallback definitions for names that mitex emits calls to but that exist
/// neither in mitex-scope nor in Typst std. (Known mitex 0.2.7 gap: `textmath`
/// is the alias target of `\text` but has no scope entry.)
const TYP_FALLBACKS: [(&str, &str); 1] = [(
    "textmath",
    "#let textmath(it) = math.upright(it)",
)];

/// Names called as `#name[` / `#name(` in the document body.
fn called_names(typ: &str) -> HashSet<String> {
    let mut names = HashSet::new();
    let b = typ.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'#' {
            let mut j = i + 1;
            while j < b.len() && (b[j].is_ascii_alphanumeric()) {
                j += 1;
            }
            if j > i + 1 {
                let mut k = j;
                while k < b.len() && (b[k] == b' ' || b[k] == b'\t') {
                    k += 1;
                }
                if k < b.len() && (b[k] == b'[' || b[k] == b'(') {
                    names.insert(typ[i + 1..j].to_string());
                }
            }
            i = j;
        } else {
            i += 1;
        }
    }
    names
}

/// Drop the first line that consists solely of a LaTeX document-class name
/// (mitex renders `\documentclass{article}` as literal `article` text).
fn strip_docclass_line(typ: &str) -> String {
    let mut out = String::with_capacity(typ.len());
    let mut dropped = false;
    for line in typ.lines() {
        if !dropped {
            let t = line.trim();
            if matches!(
                t,
                "article" | "book" | "report" | "letter" | "slides" | "memo" | "scrartcl"
                    | "scrbook" | "scrreprt" | "beamer" | "amsart" | "amsbook"
            ) {
                dropped = true;
                continue;
            }
        }
        out.push_str(line);
        out.push('\n');
    }
    out
}

pub(crate) fn postprocess_typ(
    typ: &str,
    p: &Processed,
    bib_name: Option<&str>,
    scope_keys: &HashSet<String>,
) -> String {
    // mitex emits the document-class name as literal text before the content
    // block (`article` / `book` on its own line). Drop that first line.
    let typ = strip_docclass_line(typ);
    let typ: &str = &typ;
    // Insert title block + heading numbering (+outline) after the prelude
    // (#import / #let lines at the top).
    let mut insert_at = 0;
    for (idx, line) in typ.lines().enumerate() {
        let t = line.trim_start();
        if t.starts_with("#import") || t.starts_with("#let") || t.is_empty() {
            insert_at = idx + 1;
        } else {
            break;
        }
    }
    let mut head = String::new();
    if !p.title.is_empty() || !p.author.is_empty() {
        // NOTE: `#align(center)[...]` content is dropped by typst HTML export;
        // use a block with a scoped set rule instead.
        head.push_str("#block[#set align(center);\n");
        if !p.title.is_empty() {
            head.push_str(&format!(
                "#text(size: 22pt, weight: \"bold\")[{}]\n#v(0.6em)\n",
                typst_escape(&p.title)
            ));
        }
        if !p.author.is_empty() {
            head.push_str(&format!("{}\n", typst_escape(&p.author)));
        }
        if !p.date.is_empty() {
            head.push_str(&format!("#v(0.3em)\n{}\n", typst_escape(&p.date)));
        }
        head.push_str("]\n#v(1em)\n");
    }
    head.push_str("#set heading(numbering: \"1.1\")\n");
    if p.had_toc {
        head.push_str("#outline()\n");
    }
    // Number display equations so \ref to them works (LaTeX numbers them too).
    head.push_str("#set math.equation(numbering: \"(1)\")\n");
    // Inject fallbacks for called-but-undefined names (mitex gaps).
    let called = called_names(typ);
    for (name, def) in TYP_FALLBACKS {
        if called.contains(name) && !scope_keys.contains(name) {
            head.push_str(def);
            head.push('\n');
            eprintln!("injected fallback definition for `{name}`");
        }
    }

    let mut out = String::with_capacity(typ.len() + head.len());
    let mut inserted = false;
    for (idx, line) in typ.lines().enumerate() {
        out.push_str(line);
        out.push('\n');
        if idx + 1 == insert_at {
            out.push_str(&head);
            inserted = true;
        }
    }
    if !inserted {
        out.push_str(&head);
    }
    if let Some(bib) = bib_name {
        out.push_str(&format!(
            "\n= References\n#bibliography(\"{bib}\")\n"
        ));
    }
    // Starred source headings render unnumbered. Match tolerantly: mitex
    // escapes some chars (`(` -> `\(`, `[` -> `\[`), so allow an optional
    // backslash before each non-alphanumeric char on the .typ side.
    {
        let mut seen: HashSet<(u8, &str)> = HashSet::new();
        for (level, h) in &p.unnumbered_headings {
            if !seen.insert((*level, h.as_str())) {
                continue; // duplicate title: already handled
            }
            let n = replace_heading_numbering(&mut out, *level, h);
            if n == 0 {
                warn(&format!("Could not locate starred heading `{h}` for unnumbered rendering"));
            }
        }
    }
    out
}

/// Replace `#heading(level: L)[TITLE]` with its `numbering: none` form,
/// tolerating mitex backslash-escapes in TITLE. Returns replacement count.
fn replace_heading_numbering(out: &mut String, level: u8, title: &str) -> usize {
    let marker = format!("#heading(level: {level})[");
    let replacement = format!("#heading(level: {level}, numbering: none)[");
    let mut count = 0;
    let mut res = String::with_capacity(out.len());
    let mut i = 0;
    while let Some(pos) = out[i..].find(&marker) {
        let abs = i + pos;
        let cs = abs + marker.len();
        match match_len(&out[cs..], title) {
            Some(consumed) => {
                res.push_str(&out[i..abs]);
                res.push_str(&replacement);
                res.push_str(&out[cs..cs + consumed]);
                i = cs + consumed;
                count += 1;
            }
            None => {
                res.push_str(&out[i..cs]);
                i = cs;
            }
        }
    }
    res.push_str(&out[i..]);
    *out = res;
    count
}

/// Match `title` at the start of `s`, allowing mitex to backslash-escape
/// punctuation (`(` matches `\(`). Requires `]` right after the title.
/// Returns bytes consumed in `s`.
fn match_len(s: &str, title: &str) -> Option<usize> {
    let mut si = 0;
    for c in title.chars() {
        if !(c.is_alphanumeric() || c == ' ') && s[si..].starts_with('\\') {
            si += 1;
        }
        let next = s[si..].chars().next()?;
        if next != c {
            return None;
        }
        si += next.len_utf8();
    }
    s[si..].starts_with(']').then_some(si)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_match_len() {
        assert_eq!(match_len("Intro]", "Intro"), Some(5));
        assert_eq!(match_len("A \\(B\\)]", "A (B)"), Some(7));
        assert_eq!(match_len("Intro X]", "Intro"), None);
    }

    #[test]
    fn test_prune_prelude() {
        let typ = "#import \"@preview/mitex:0.2.7\": *\n#let frac = mitex-scope.at(\"frac\", default: none);\n#let emph = mitex-scope.at(\"emph\", default: none);\nbody\n";
        let mut keys = std::collections::HashSet::new();
        keys.insert("frac".to_string());
        let (pruned, dropped) = prune_prelude(typ, &keys);
        assert_eq!(dropped, vec!["emph".to_string()]);
        assert!(pruned.contains("#let frac"), "kept: {pruned}");
        assert!(!pruned.contains("#let emph"), "dropped: {pruned}");
        assert!(pruned.contains("body"), "body kept: {pruned}");
    }

    #[test]
    fn test_mitex_import_spec() {
        assert_eq!(
            mitex_import_spec("#import \"@preview/mitex:0.2.7\": *\n"),
            Some("@preview/mitex:0.2.7".to_string())
        );
        assert_eq!(mitex_import_spec("no import here"), None);
    }

    fn sample_processed() -> Processed {
        Processed {
            text: String::new(),
            title: "T".to_string(),
            author: "A".to_string(),
            date: String::new(),
            had_toc: true,
            unnumbered_headings: vec![(3u8, "Detail".to_string())],
            generated_assets: Vec::new(),
        }
    }

    #[test]
    fn test_postprocess_inserts_head() {
        let typ = "#import \"@preview/mitex:0.2.7\": *\n#let frac = 1;\nHello\n";
        let keys = std::collections::HashSet::new();
        let out = postprocess_typ(typ, &sample_processed(), None, &keys);
        assert!(out.contains("#text(size: 22pt"), "title: {out}");
        assert!(out.contains("#set heading(numbering: \"1.1\")"), "numbering: {out}");
        assert!(out.contains("#outline()"), "outline: {out}");
        assert!(out.contains("#set math.equation(numbering: \"(1)\")"), "eqnum: {out}");
        assert!(out.contains("Hello"), "body: {out}");
    }

    #[test]
    fn test_postprocess_bib_and_unnumbered() {
        let typ = "#import \"x\": *\n#heading(level: 3)[Detail];\n";
        let keys = std::collections::HashSet::new();
        let out = postprocess_typ(typ, &sample_processed(), Some("r.bib"), &keys);
        assert!(out.contains("#bibliography(\"r.bib\")"), "bib: {out}");
        assert!(
            out.contains("#heading(level: 3, numbering: none)[Detail]"),
            "unnumbered: {out}"
        );
    }

    #[test]
    fn test_postprocess_fallback_injection() {
        // textmath is called but missing from scope -> fallback defined.
        let typ = "#import \"x\": *\n#textmath[hi];\n";
        let p = Processed {
            text: String::new(),
            title: String::new(),
            author: String::new(),
            date: String::new(),
            had_toc: false,
            unnumbered_headings: Vec::new(),
            generated_assets: Vec::new(),
        };
        let keys = std::collections::HashSet::new();
        let out = postprocess_typ(typ, &p, None, &keys);
        assert!(out.contains("#let textmath(it) = math.upright(it)"), "fallback: {out}");
        // Present in scope -> no injection.
        let mut keys2 = std::collections::HashSet::new();
        keys2.insert("textmath".to_string());
        let out2 = postprocess_typ(typ, &p, None, &keys2);
        assert!(!out2.contains("#let textmath"), "no dup: {out2}");
    }
}
