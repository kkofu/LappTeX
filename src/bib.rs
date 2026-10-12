use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use crate::tex::{
    find_balanced, match_command_at, parse_braced, read_file_lossy, skip_star_brackets, skip_ws,
    strip_comments,
};

// ---------------------------------------------------------------------------
// Bibliography synthesis from .bbl
// ---------------------------------------------------------------------------

pub(crate) fn collect_cite_keys(text: &str) -> Vec<String> {
    if !text.contains("\\cite") {
        return Vec::new();
    }
    let mut keys = Vec::new();
    let mut seen = HashSet::new();
    let mut i = 0;
    while let Some(pos) = text[i..].find("\\cite") {
        let abs = i + pos;
        let after = match match_command_at(text, abs, &["cite"]) {
            Some((_, after)) => after,
            None => {
                i = abs + "\\cite".len();
                continue;
            }
        };
        let j = skip_star_brackets(text, after);
        if let Some((arg, next_i)) = parse_braced(text, j) {
            for k in arg.split(',') {
                let k = k.trim().to_string();
                if !k.is_empty() && seen.insert(k.clone()) {
                    keys.push(k);
                }
            }
            i = next_i;
        } else {
            i = after;
        }
    }
    keys
}

fn clean_bib_value(v: &str) -> String {
    // Remove biblatex helper macros, keep group contents.
    let mut s = v.to_string();
    for (from, to) in [
        ("\\bibinitperiod", "."),
        ("\\bibnamedelima", " "),
        ("\\bibinitdelim", " "),
        ("\\bibinithyphendelim", "-"),
        ("\\bibrangedash", "--"),
        ("\\nolinebreak", ""),
        ("\\labelalphaothers", "+"),
    ] {
        s = s.replace(from, to);
    }
    s = expand_bbl_verb(&s);
    // bbl uses % as line-continuation/comment; a bare % would comment out the
    // rest of the .bib line, so drop comments (respecting \% escapes).
    s = strip_comments(&s);
    // Strip remaining \command tokens (keep their {args} content), but preserve
    // single-char escapes like \& \% that BibLaTeX understands.
    let mut out = String::with_capacity(s.len());
    let b = s.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'\\' && i + 1 < b.len() {
            if b[i + 1].is_ascii_alphabetic() {
                let mut j = i + 1;
                while j < b.len() && b[j].is_ascii_alphabetic() {
                    j += 1;
                }
                i = j;
                continue;
            } else {
                // Keep \<char> escape as-is.
                out.push('\\');
                let ch = s[i + 1..].chars().next().unwrap();
                out.push(ch);
                i += 1 + ch.len_utf8();
                continue;
            }
        }
        let ch = s[i..].chars().next().unwrap();
        out.push(ch);
        i += ch.len_utf8();
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Expand bbl `\verb<delim>...<delim>` inline verbatim to its content.
fn expand_bbl_verb(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    let b = s.as_bytes();
    while let Some(pos) = s[i..].find("\\verb") {
        let abs = i + pos;
        out.push_str(&s[i..abs]);
        let mut j = abs + "\\verb".len();
        let d = b.get(j).copied().unwrap_or(0);
        if d == 0 {
            break;
        }
        j += 1;
        if d == b'{' {
            // \verb{...}: balanced to closing brace.
            if let Some(close) = find_balanced(s, j - 1) {
                out.push_str(&s[j..close]);
                i = close + 1;
            } else {
                i = j;
            }
        } else if d == b' ' || d == b'\t' || d == b'\n' {
            // \verb<space>value: read to end of line.
            let mut k = j;
            while k < b.len() && b[k] != b'\n' {
                k += 1;
            }
            out.push_str(s[j..k].trim());
            i = k;
        } else {
            // \verb<delim>value<delim>.
            let mut k = j;
            while k < b.len() && b[k] != d {
                k += 1;
            }
            out.push_str(&s[j..k]);
            i = if k < b.len() { k + 1 } else { k };
        }
    }
    out.push_str(&s[i..]);
    out
}

fn extract_field(block: &str, kind: &str, name: &str) -> Option<String> {
    // \field{name}{...} or \list{name}{n}{...}
    let needle = format!("\\{kind}{{{name}}}");
    let pos = block.find(&needle)?;
    let mut j = skip_ws(block, pos + needle.len());
    if kind == "list" {
        // \list{name}{count}{...}: skip the count group.
        if block.as_bytes().get(j) == Some(&b'{') {
            let close = find_balanced(block, j)?;
            j = skip_ws(block, close + 1);
        }
    }
    let (arg, _) = parse_braced(block, j)?;
    Some(clean_bib_value(arg))
}

fn extract_names(block: &str, role: &str) -> Option<String> {
    let needle = format!("\\name{{{role}}}");
    let pos = block.find(&needle)?;
    let mut j = skip_ws(block, pos + needle.len());
    // \name{role}{count}{opts}{...}: skip count and opts groups.
    for _ in 0..2 {
        if block.as_bytes().get(j) == Some(&b'{') {
            if let Some(close) = find_balanced(block, j) {
                j = skip_ws(block, close + 1);
            }
        }
    }
    let (group, _) = parse_braced(block, j)?;
    // Collect family/given pairs in order (balanced-brace aware so that
    // double-braced corporate authors like family={{Name}} stay balanced).
    let mut names = Vec::new();
    let mut k = 0;
    while let Some(fp) = group[k..].find("family=") {
        let mut f_abs = k + fp + "family=".len();
        if group.as_bytes().get(f_abs) != Some(&b'{') {
            k = f_abs;
            continue;
        }
        let f_end = match find_balanced(group, f_abs) {
            Some(e) => e,
            None => break,
        };
        let family = clean_bib_value(&group[f_abs + 1..f_end]);
        f_abs = f_end + 1;
        let given = match group[f_abs..].find("given=") {
            Some(gp) => {
                // Only accept if it comes before the next family= (same author).
                let g_abs = f_abs + gp + "given=".len();
                let next_fam = group[f_abs..].find("family=").map(|p| f_abs + p);
                if next_fam.map(|nf| g_abs < nf).unwrap_or(true)
                    && group.as_bytes().get(g_abs) == Some(&b'{')
                {
                    match find_balanced(group, g_abs) {
                        Some(ge) => clean_bib_value(&group[g_abs + 1..ge]),
                        None => String::new(),
                    }
                } else {
                    String::new()
                }
            }
            None => String::new(),
        };
        if !family.is_empty() {
            if given.is_empty() {
                names.push(family);
            } else {
                names.push(format!("{family}, {given}"));
            }
        }
        k = f_end + 1;
    }
    if names.is_empty() {
        None
    } else {
        Some(names.join(" and "))
    }
}

fn map_bib_type(t: &str) -> &str {
    match t {
        "book" | "article" | "inproceedings" | "proceedings" | "incollection" | "collection"
        | "thesis" | "online" | "misc" | "unpublished" | "manual" | "report" | "booklet"
        | "inbook" | "suppbook" | "mvbook" | "techreport" => t,
        "phdthesis" | "mathesis" | "mastersthesis" => "thesis",
        _ => "misc",
    }
}

pub(crate) fn synth_bib_from_bbl(bbl: &str, keys: &[String]) -> (String, Vec<String>) {
    // Split into \entry{key}{type}{} blocks.
    let mut entries: HashMap<String, (String, String)> = HashMap::new();
    let mut order: Vec<String> = Vec::new();
    let mut pos = 0;
    while let Some(p) = bbl[pos..].find("\\entry{") {
        let abs = pos + p;
        let key_start = abs + "\\entry{".len();
        let key_end = match bbl[key_start..].find('}') {
            Some(e) => key_start + e,
            None => break,
        };
        let key = bbl[key_start..key_end].to_string();
        let type_start = key_end + 1;
        let (etype, _) = parse_braced(bbl, type_start).unwrap_or(("", type_start));
        // Block ends at next \entry{ or \enddatalist / \endrefsection.
        let rest = &bbl[key_end..];
        let mut end = rest.len();
        for marker in ["\\entry{", "\\enddatalist", "\\endrefsection"] {
            if let Some(mp) = rest.find(marker) {
                end = end.min(mp);
            }
        }
        let block = &rest[..end];
        entries.insert(key.clone(), (etype.to_string(), block.to_string()));
        order.push(key);
        pos = key_end + end;
    }

    let mut bib = String::new();
    let mut missing = Vec::new();
    for key in keys {
        match entries.get(key) {
            Some((etype, block)) => {
                let btype = map_bib_type(etype);
                bib.push_str(&format!("@{btype}{{{key},\n"));
                let author =
                    extract_names(block, "author").or_else(|| extract_names(block, "editor"));
                if let Some(a) = author {
                    bib.push_str(&format!("  author = {{{a}}},\n"));
                }
                for (f, needs_list) in [
                    ("title", false),
                    ("journaltitle", false),
                    ("publisher", true),
                    ("volume", false),
                    ("number", false),
                    ("pages", false),
                    ("doi", false),
                    ("url", false),
                ] {
                    let v = if needs_list {
                        extract_field(block, "list", f)
                    } else {
                        extract_field(block, "field", f)
                    };
                    if let Some(v) = v {
                        if !v.is_empty() {
                            let name = if f == "journaltitle" { "journal" } else { f };
                            bib.push_str(&format!("  {name} = {{{v}}},\n"));
                        }
                    }
                }
                let year = extract_field(block, "field", "year").or_else(|| {
                    extract_field(block, "field", "date").map(|d| {
                        d.split('-').next().unwrap_or("").trim().to_string()
                    })
                });
                if let Some(y) = year {
                    if !y.is_empty() && y.chars().all(|c| c.is_ascii_digit()) {
                        bib.push_str(&format!("  year = {{{y}}},\n"));
                    }
                }
                bib.push_str("}\n\n");
            }
            None => {
                missing.push(key.clone());
                bib.push_str(&format!("@misc{{{key},\n  author = {{Unknown}},\n  title = {{{key}}}\n}}\n\n"));
            }
        }
    }
    (bib, missing)
}

pub(crate) fn find_existing_bib(root: &Path, keys: &[String]) -> Option<String> {
    // Look for any .bib in root that contains all keys.
    let entries = std::fs::read_dir(root).ok()?;
    for e in entries.flatten() {
        let p = e.path();
        if p.extension().is_some_and(|x| x == "bib") {
            let content = read_file_lossy(&p).unwrap_or_default();
            if keys.iter().all(|k| content.contains(k)) {
                return p
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned());
            }
        }
    }
    None
}

pub(crate) fn first_bbl(root: &Path) -> Option<PathBuf> {
    let entries = std::fs::read_dir(root).ok()?;
    for e in entries.flatten() {
        let p = e.path();
        if p.extension().is_some_and(|x| x == "bbl") {
            return Some(p);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE_BBL: &str = r#"% $ biblatex auxiliary file $
\refsection{0}
  \datalist[entry]{anyt//global/global}
    \entry{doe2024}{book}{}
      \name{author}{1}{}{%
        {{hash=abc}{%
           family={Doe},
           familyi={D\bibinitperiod},
           given={John},
           giveni={J\bibinitperiod}}}%
      }
      \list{publisher}{1}{%
        {Acme Press}%
      }
      \field{title}{Some Title}
      \field{year}{2024}
    \entry{web2024}{online}{}
      \field{title}{A Page}
      \field{date}{2024-05-01}
      \field{url}{\verb{https://example.com/x}}
    \entry{corp2023}{report}{}
      \name{author}{1}{}{%
        {{hash=def}{%
           family={{Hagen von Eitzen}},
           familyi={H\bibinitperiod}}}%
      }
      \field{title}{Note}
    \endentry
  \enddatalist
\endrefsection
"#;

    #[test]
    fn test_synth_full_and_stub() {
        let keys = vec!["doe2024".to_string(), "web2024".to_string(), "missing".to_string()];
        let (bib, missing) = synth_bib_from_bbl(SAMPLE_BBL, &keys);
        assert_eq!(missing, vec!["missing".to_string()]);
        assert!(bib.contains("@book{doe2024,"), "book entry: {bib}");
        assert!(bib.contains("author = {Doe, John}"), "author: {bib}");
        assert!(bib.contains("publisher = {{Acme Press}}"), "publisher: {bib}");
        assert!(bib.contains("year = {2024}"), "year: {bib}");
        // Date falls back to year part.
        assert!(bib.contains("@online{web2024,"), "online type: {bib}");
        assert!(bib.contains("year = {2024}"), "date year: {bib}");
        assert!(bib.contains("https://example.com/x"), "verb url: {bib}");
        // Missing key -> stub.
        assert!(bib.contains("@misc{missing,"), "stub: {bib}");
        assert!(bib.contains("author = {Unknown}"), "stub author: {bib}");
    }

    #[test]
    fn test_synth_corporate_author() {
        let keys = vec!["corp2023".to_string()];
        let (bib, missing) = synth_bib_from_bbl(SAMPLE_BBL, &keys);
        assert!(missing.is_empty());
        assert!(
            bib.contains("author = {{Hagen von Eitzen}}"),
            "corporate braces kept: {bib}"
        );
    }

    #[test]
    fn test_clean_bib_value() {
        assert_eq!(clean_bib_value("T\\bibinitperiod M"), "T. M");
        assert_eq!(clean_bib_value("a\\bibrangedash b"), "a-- b");
        assert_eq!(clean_bib_value("x% comment"), "x");
        assert_eq!(clean_bib_value("100\\% sure"), "100\\% sure");
    }

    #[test]
    fn test_map_bib_type() {
        assert_eq!(map_bib_type("phdthesis"), "thesis");
        assert_eq!(map_bib_type("article"), "article");
        assert_eq!(map_bib_type("weird"), "misc");
    }

    #[test]
    fn test_collect_cite_keys() {
        let keys = collect_cite_keys("a \\cite{x, y} b \\cite{x} c \\cite*[p]{z}");
        assert_eq!(keys, vec!["x".to_string(), "y".to_string(), "z".to_string()]);
    }
}
