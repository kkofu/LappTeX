use std::collections::HashMap;

use crate::refs::sanitize_label_key;
use crate::tex::{apply_edits, find_bracket, is_name_char, match_command_at, parse_braced, skip_brackets, skip_ws};

/// Theorem-like environments: (env, display word, counter group 0/1/2).
const THM_ENVS: [(&str, &str, usize); 7] = [
    ("theorem", "Theorem", 0),
    ("lemma", "Lemma", 0),
    ("corollary", "Corollary", 0),
    ("conjecture", "Conjecture", 0),
    ("proposition", "Proposition", 0),
    ("definition", "Definition", 1),
    ("remark", "Remark", 2),
];

/// Label prefixes that are NEVER theorem labels (left for typst ref()).
fn is_non_thm_label(key: &str) -> bool {
    let pre = key.split(':').next().unwrap_or("");
    matches!(pre, "eqn" | "eq" | "fig" | "sec" | "tab" | "appendix")
}

/// Number theorem-like environments LaTeX-style (L1.L2.n within section) and
/// resolve references to their labels as static text, because Typst cannot
/// reference plain-text content.
///
/// `book_style` selects three-part (chapter docs) vs two-part (article docs)
/// numbering. Returns (rewritten_body, label -> (display_word, number)).
pub(crate) fn number_theorems(
    body: &str,
    book_style: bool,
) -> (String, HashMap<String, (String, String)>) {
    struct Occ {
        begin_start: usize,
        begin_end: usize, // end of \begin{env}[opt]
        end_start: usize,
        end_end: usize,
        word: &'static str,
        num: String,
        opt: String,
    }
    // Phase A: scan headings for counters + collect env occurrences.
    let mut l1: i32 = 0;
    let mut l2: i32 = 0;
    let mut counters: HashMap<(i32, i32), [i32; 3]> = HashMap::new();
    let mut occs: Vec<Occ> = Vec::new();
    // (label_span_start, label_span_end, key)
    let mut label_spans: Vec<(usize, usize, String)> = Vec::new();

    let b = body.as_bytes();
    let mut i = 0;
    while i < b.len() {
        let rest = &body[i..];
        // Headings (already demoted: section=L1, subsection=L2 in books;
        // in articles section=L2 and deeper levels don't reset counters).
        let mut consumed = false;
        for (cmd, is_l1) in [("\\section", book_style), ("\\subsection", false)] {
            if rest.starts_with(cmd) {
                let after = i + cmd.len();
                let next = b.get(after).copied().unwrap_or(b' ');
                if is_name_char(next) {
                    break;
                }
                let mut j = after;
                if b.get(j) == Some(&b'*') {
                    j += 1;
                }
                if let Some((_, next_i)) = parse_braced(body, j) {
                    if is_l1 {
                        l1 += 1;
                        l2 = 0;
                    } else if book_style || cmd == "\\section" {
                        // book subsection, or article section
                        l2 += 1;
                    }
                    // article subsection and below: no counter changes
                    i = next_i;
                    consumed = true;
                    break;
                }
            }
        }
        if consumed {
            continue;
        }
        // Theorem envs.
        let mut found_env: Option<(&str, &str, usize)> = None;
        for e in &THM_ENVS {
            let begin = format!("\\begin{{{}}}", e.0);
            if rest.starts_with(&begin) {
                found_env = Some((e.0, e.1, e.2));
                break;
            }
        }
        if let Some((env, word, group)) = found_env {
            let begin_start = i;
            let mut j = skip_ws(body, i + format!("\\begin{{{env}}}").len());
            let opt = if b.get(j) == Some(&b'[') {
                match find_bracket(body, j) {
                    Some(close) => {
                        let o = body[j + 1..close].trim().to_string();
                        j = close + 1;
                        o
                    }
                    None => String::new(),
                }
            } else {
                String::new()
            };
            let end_tag = format!("\\end{{{env}}}");
            let inner_start = j;
            let (end_start, end_end) = match body[j..].find(&end_tag) {
                Some(p) => (j + p, j + p + end_tag.len()),
                None => {
                    i += 1;
                    continue; // unclosed: skip (mitex will report it)
                }
            };
            let cnt = counters.entry((l1, l2)).or_insert([0, 0, 0]);
            cnt[group] += 1;
            let num = if book_style {
                format!("{l1}.{l2}.{}", cnt[group])
            } else {
                format!("{l2}.{}", cnt[group])
            };
            // Labels inside (excluding eqn/fig/sec labels).
            let inner = &body[inner_start..end_start];
            let mut k = 0;
            while let Some(p) = inner[k..].find("\\label") {
                let labs = k + p;
                let lafter = labs + "\\label".len();
                let lnext = inner.as_bytes().get(lafter).copied().unwrap_or(b' ');
                if is_name_char(lnext) {
                    k = lafter;
                    continue;
                }
                if let Some((arg, _)) = parse_braced(inner, lafter) {
                    let key = arg.trim().to_string();
                    if !key.is_empty() && !is_non_thm_label(&key) {
                        // Absolute span in body coordinates.
                        let abs_labs = inner_start + labs;
                        // Span covers \label{...}: recompute end via body-level parse.
                        if let Some((_, abs_end)) = parse_braced(body, abs_labs + "\\label".len()) {
                            label_spans.push((abs_labs, abs_end, key));
                        }
                    }
                    k = lafter + arg.len() + 2;
                } else {
                    k = lafter;
                }
            }
            occs.push(Occ {
                begin_start,
                begin_end: inner_start,
                end_start,
                end_end,
                word,
                num,
                opt,
            });
            i = end_end;
            continue;
        }
        let ch = rest.chars().next().unwrap();
        i += ch.len_utf8();
    }

    // Build label -> (word, num) map.
    let mut map: HashMap<String, (String, String)> = HashMap::new();
    for (s, _, key) in &label_spans {
        for o in &occs {
            if o.begin_end <= *s && *s < o.end_start {
                map.insert(sanitize_label_key(key), (o.word.to_string(), o.num.clone()));
                break;
            }
        }
    }

    // Phase B: rebuild with headers / dropped ends / dropped labels.
    let mut edits: Vec<(usize, usize, String)> = Vec::new(); // (start, end, replacement)
    for o in &occs {
        let opt_str = if o.opt.is_empty() {
            ".".to_string()
        } else {
            format!(" ({}).", o.opt)
        };
        edits.push((
            o.begin_start,
            o.begin_end,
            format!("\n\n\\textbf{{{} {}{opt_str}}} ", o.word, o.num),
        ));
        edits.push((o.end_start, o.end_end, String::new()));
    }
    for (s, e, _) in &label_spans {
        edits.push((*s, *e, String::new()));
    }
    let out = apply_edits(body, &mut edits);

    // Phase C: static-ize references to mapped labels.
    let out = staticize_thm_refs(&out, &map);
    (out, map)
}

/// Replace ref-commands whose keys are ALL in `map` with static text.
pub(crate) fn staticize_thm_refs(body: &str, map: &HashMap<String, (String, String)>) -> String {
    let mut out = String::with_capacity(body.len());
    let mut i = 0;
    let b = body.as_bytes();
    let cmds = ["Cref", "cref", "autoref", "Autoref", "ref", "eqref"];
    let mut mixed_left = 0;
    while i < b.len() {
        if b[i] == b'\\' {
            let hit = match_command_at(body, i, &cmds);
            if let Some((idx, after)) = hit {
                let cmd = cmds[idx];
                let mut j = skip_ws(body, after);
                let mut starred = false;
                if b.get(j) == Some(&b'*') {
                    starred = true;
                    j = skip_ws(body, j + 1);
                }
                let prev_j = j;
                j = skip_brackets(body, j);
                let bracketed = j != prev_j;
                if let Some((arg, next_i)) = parse_braced(body, j) {
                    let keys: Vec<String> = arg
                        .split(',')
                        .map(|k| sanitize_label_key(k.trim()))
                        .collect();
                    if !bracketed && !starred && keys.iter().all(|k| map.contains_key(k)) {
                        let parts: Vec<String> = keys
                            .iter()
                            .map(|k| {
                                let (w, n) = &map[k];
                                match cmd {
                                    "Cref" | "autoref" | "Autoref" => format!("{w} {n}"),
                                    "cref" => format!("{} {n}", lowercase_first(w)),
                                    "eqref" => format!("({n})"),
                                    _ => n.clone(),
                                }
                            })
                            .collect();
                        out.push_str(&parts.join(", "));
                        i = next_i;
                        continue;
                    } else if keys.iter().any(|k| map.contains_key(k)) {
                        mixed_left += 1;
                    }
                }
            }
        }
        let ch = body[i..].chars().next().unwrap();
        out.push(ch);
        i += ch.len_utf8();
    }
    if mixed_left > 0 {
        eprintln!("left {mixed_left} mixed static/dynamic reference(s) for generic handling");
    }
    out
}

fn lowercase_first(s: &str) -> String {
    let mut chs = s.chars();
    match chs.next() {
        Some(c) => c.to_lowercase().collect::<String>() + chs.as_str(),
        None => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn thm_map() -> HashMap<String, (String, String)> {
        let mut m = HashMap::new();
        m.insert("a".to_string(), ("Theorem".to_string(), "1.2.3".to_string()));
        m
    }

    #[test]
    fn test_staticize_all_forms() {
        let map = thm_map();
        assert_eq!(staticize_thm_refs("\\Cref{a}", &map), "Theorem 1.2.3");
        assert_eq!(staticize_thm_refs("\\cref{a}", &map), "theorem 1.2.3");
        assert_eq!(staticize_thm_refs("\\autoref{a}", &map), "Theorem 1.2.3");
        assert_eq!(staticize_thm_refs("\\ref{a}", &map), "1.2.3");
        assert_eq!(staticize_thm_refs("\\eqref{a}", &map), "(1.2.3)");
    }

    #[test]
    fn test_staticize_leaves_unknown_starred_bracketed() {
        let map = thm_map();
        // Unknown key: untouched.
        assert_eq!(staticize_thm_refs("\\Cref{b}", &map), "\\Cref{b}");
        // Mixed known/unknown: untouched (generic pass handles it).
        assert_eq!(staticize_thm_refs("\\Cref{a,b}", &map), "\\Cref{a,b}");
        // Starred / bracketed: untouched.
        assert_eq!(staticize_thm_refs("\\Cref*{a}", &map), "\\Cref*{a}");
        assert_eq!(staticize_thm_refs("\\Cref[x]{a}", &map), "\\Cref[x]{a}");
        // Multi-key all known: joined.
        assert_eq!(staticize_thm_refs("\\Cref{a,a}", &map), "Theorem 1.2.3, Theorem 1.2.3");
    }

    #[test]
    fn test_number_theorems_book() {
        let body = "\\section{A}\n\\begin{theorem}\\label{thm:x}Body\\end{theorem}\n\\Cref{thm:x}\n\\section{B}\n\\begin{lemma}\\label{lem:y}L\\end{lemma}";
        let (out, map) = number_theorems(body, true);
        assert_eq!(map.get("thm:x"), Some(&("Theorem".to_string(), "1.0.1".to_string())));
        assert_eq!(map.get("lem:y"), Some(&("Lemma".to_string(), "2.0.1".to_string())));
        assert!(out.contains("\\textbf{Theorem 1.0.1.}"), "header missing: {out}");
        assert!(out.contains("Theorem 1.0.1"), "static ref missing: {out}");
        assert!(!out.contains("\\label{thm:x}"), "label not dropped: {out}");
        assert!(!out.contains("\\begin{theorem}"), "env not dropped: {out}");
    }

    #[test]
    fn test_number_theorems_article_groups_and_opt() {
        let body = "\\section{S}\n\\begin{definition}[Named]\\label{def:d}D\\end{definition}\n\\begin{theorem}\\label{thm:t}T\\end{theorem}";
        let (out, map) = number_theorems(body, false);
        // Article style: two-part numbers; definition/remark have own counters.
        assert_eq!(map.get("def:d"), Some(&("Definition".to_string(), "1.1".to_string())));
        assert_eq!(map.get("thm:t"), Some(&("Theorem".to_string(), "1.1".to_string())));
        assert!(out.contains("\\textbf{Definition 1.1 (Named).}"), "opt header: {out}");
    }

    #[test]
    fn test_number_theorems_skips_non_thm_labels() {
        let body = "\\section{S}\n\\begin{theorem}See \\ref{fig:f}.\\label{eq:x}\\end{theorem}";
        let (out, map) = number_theorems(body, false);
        assert!(!map.contains_key("eq:x"), "eq label must not map: {out}");
        assert!(out.contains("\\label{eq:x}"), "eq label must survive: {out}");
    }

    #[test]
    fn test_lowercase_first() {
        assert_eq!(lowercase_first("Theorem"), "theorem");
        assert_eq!(lowercase_first(""), "");
    }
}

