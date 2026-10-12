use std::path::Path;

use crate::cmd::warn;
use crate::envs::{
    drop_env, drop_iffalse, extract_algorithms, flatten_subfigures, hoist_math_labels,
    number_tables, rewrite_longtable, rewrite_proof, rewrite_tables, strip_env,
    strip_env_options,
};
use crate::graphics::resolve_graphics;
use crate::refs::{
    keep_last_arg, rename_command_bare, rename_command_with_arg, rewrite_crefrange,
    rewrite_ref_like, sanitize_label_args, split_multicite, texorpdfstring_to_first,
};
use crate::tex::{find_balanced, find_bracket, parse_braced, rewrite_cmd, skip_ws, CmdEdit};
use crate::theorems::{number_theorems, staticize_thm_refs};

// ---------------------------------------------------------------------------
// Preprocessing: preamble split + rewrites
// ---------------------------------------------------------------------------

pub(crate) struct Processed {
    pub(crate) text: String,
    pub(crate) title: String,
    pub(crate) author: String,
    pub(crate) date: String,
    pub(crate) had_toc: bool,
    /// Heading texts that were starred in source (render unnumbered).
    pub(crate) unnumbered_headings: Vec<(u8, String)>,
    /// Files generated during preprocessing (converted figures) for cleanup.
    pub(crate) generated_assets: Vec<std::path::PathBuf>,
}

fn extract_command_arg(preamble: &str, name: &str) -> Option<String> {
    // Finds \name ... {balanced}, returns innermost content.
    let mut search = 0;
    let needle = format!("\\{name}");
    while let Some(pos) = preamble[search..].find(&needle) {
        let abs = search + pos;
        let after = abs + needle.len();
        let next = preamble.as_bytes().get(after).copied().unwrap_or(b' ');
        if next.is_ascii_alphanumeric() || next == b'*' || next == b'@' {
            search = after;
            continue;
        }
        if let Some((arg, _)) = parse_braced(preamble, after) {
            return Some(arg.to_string());
        }
        search = after;
    }
    None
}

/// Remove balanced occurrences of `\name{...}` (with optional `[...]` before) from text.
fn remove_command_with_arg(text: &str, name: &str) -> String {
    rewrite_cmd(text, name, |h| {
        // Historical boundary: a trailing `*` keeps the command.
        if h.body[h.after..].starts_with('*') {
            return CmdEdit::Keep;
        }
        let mut j = skip_ws(h.body, h.after);
        if h.body.as_bytes().get(j) == Some(&b'[') {
            if let Some(close) = find_bracket(h.body, j) {
                j = skip_ws(h.body, close + 1);
            }
        }
        if h.body.as_bytes().get(j) == Some(&b'{') {
            if let Some(close) = find_balanced(h.body, j) {
                return CmdEdit::Replace(String::new(), close + 1);
            }
        }
        // No braced arg: drop just the command name.
        CmdEdit::Replace(String::new(), h.after)
    })
}

/// Remove bare `\name` occurrences (no argument), e.g. `\tableofcontents`.
fn remove_bare_command(text: &str, name: &str) -> String {
    rewrite_cmd(text, name, |h| {
        if h.body[h.after..].starts_with('*') {
            return CmdEdit::Keep;
        }
        CmdEdit::Replace(String::new(), h.after)
    })
}

/// True if `\name` (exact command, not `\namefoo`, not `\\name` text) occurs.
pub(crate) fn has_command(body: &str, name: &str) -> bool {
    let needle = format!("\\{name}");
    let b = body.as_bytes();
    let mut k = 0;
    while let Some(p) = body[k..].find(&needle) {
        let abs = k + p;
        let prev_ok = abs == 0 || b[abs - 1] != b'\\';
        let after = abs + needle.len();
        let next = b.get(after).copied().unwrap_or(b' ');
        if prev_ok && !(next.is_ascii_alphanumeric() || next == b'@') {
            return true;
        }
        k = after;
    }
    false
}

pub(crate) fn preprocess(
    bundled: &str,
    root: &Path,
    book_style: bool,
) -> Result<Processed, String> {
    // Split preamble / body at \begin{document}.
    let doc_begin = bundled
        .find("\\begin{document}")
        .ok_or_else(|| "No \\begin{document} found in bundled source".to_string())?;
    let preamble = &bundled[..doc_begin];
    let mut body = bundled[doc_begin + "\\begin{document}".len()..].to_string();
    if let Some(pos) = body.find("\\end{document}") {
        body.truncate(pos);
    }

    // Keep \newtheorem definitions (required for theorem-like envs), drop the rest.
    let mut kept_preamble = String::new();
    for line in preamble.lines() {
        let t = line.trim_start();
        if t.starts_with("\\newtheorem") {
            kept_preamble.push_str(line);
            kept_preamble.push('\n');
        }
    }
    let title = extract_command_arg(preamble, "title").unwrap_or_default();
    let author = extract_command_arg(preamble, "author").unwrap_or_default();
    let date = extract_command_arg(preamble, "date").unwrap_or_default();
    // Clean author/date extras.
    let mut author = remove_command_with_arg(&author, "thanks");
    let mut title = title;
    let mut date = date.replace("\\today", "");
    author = author.replace('~', " ");
    date = date.replace('~', " ");
    // Drop graphics/logic commands that may sit in title/author (e.g. logos).
    for cmd in ["raisebox", "includegraphics", "textcolor", "color"] {
        title = remove_command_with_arg(&title, cmd);
        author = remove_command_with_arg(&author, cmd);
    }
    title = title.split_whitespace().collect::<Vec<_>>().join(" ");
    author = author.split_whitespace().collect::<Vec<_>>().join(" ");

    // ---- structural rewrites (order matters) ----
    // NOTE: heading demotion (chapter->section etc.) is done by demote_headings()
    // on the bundled text BEFORE this function, in a single pass to avoid cascading.
    // \begin{abstract} -> unnumbered "Abstract" section (else untitled).
    let had_abstract = body.contains("\\begin{abstract}");
    if had_abstract {
        body = body.replace("\\begin{abstract}", "\\section{Abstract}");
        body = body.replace("\\end{abstract}", "");
    }
    // Algorithm envs: keep caption as header text, resolve labels statically.
    let (body, alg_map) = extract_algorithms(&body);
    if !alg_map.is_empty() {
        eprintln!("extracted {} algorithm(s)", alg_map.len());
    }
    // TikZ pictures cannot be transpiled: drop them (captions/labels live outside).
    let (body, dropped_tikz) = drop_env(&body, "tikzpicture");
    if dropped_tikz > 0 {
        eprintln!("dropped {dropped_tikz} tikzpicture env(s)");
    }
    // Subfigures: flatten inner captions to `(a) text`, map inner labels.
    let (body, subfig_map) = flatten_subfigures(&body);
    let mut body = body;
    // NOTE: manual ref-prefix stripping (Fig./section) runs in convert(),
    // before paragraph refs go static.
    // Math labels must sit inside math for Typst to reference them.
    body = hoist_math_labels(&body);
    // Theorem numbering + static references (consumes raw \Cref forms).
    let (mut body, thm_map) = number_theorems(&body, book_style);
    if !thm_map.is_empty() {
        eprintln!("resolved {} theorem-like labels statically", thm_map.len());
    }
    // Second staticize pass for algorithm/subfigure/table labels.
    // (Table bodies stay as Typst tables; only references go static.)
    let tab_map = number_tables(&body);
    if !tab_map.is_empty() {
        eprintln!("resolved {} table label(s) statically", tab_map.len());
    }
    if !alg_map.is_empty() || !subfig_map.is_empty() || !tab_map.is_empty() {
        let mut merged = thm_map;
        merged.extend(alg_map);
        merged.extend(subfig_map);
        merged.extend(tab_map);
        body = staticize_thm_refs(&body, &merged);
    }

    let had_toc = body.contains("\\tableofcontents");
    for bare in [
        "frontmatter",
        "mainmatter",
        "backmatter",
        "appendix",
        "maketitle",
        "tableofcontents",
        "protect",
        "tightlist",
        "raggedright",
        "arraybackslash",
        "clearpage",
        "newpage",
        "hfill",
        "nonumber",
        "begingroup",
        "endgroup",
        "nointerlineskip",
    ] {
        body = remove_bare_command(&body, bare);
    }
    body = remove_command_with_arg(&body, "noalign");
    body = remove_command_with_arg(&body, "printbibliography");
    body = remove_command_with_arg(&body, "bibliography");
    body = remove_command_with_arg(&body, "addbibresource");
    // Spacing/page commands with arguments.
    for cmd in ["vspace", "hspace", "cmidrule"] {
        body = remove_command_with_arg(&body, cmd);
    }

    // Drop whole lines: \addcontentsline, \addtocontents, \def, \definecolor,
    // \setlength, \addtolength, \newcolumntype (single pass).
    body = body
        .lines()
        .filter(|l| {
            let t = l.trim_start();
            !(t.starts_with("\\addcontentsline")
                || t.starts_with("\\addtocontents")
                || t.starts_with("\\def")
                || t.starts_with("\\definecolor")
                || t.starts_with("\\setlength")
                || t.starts_with("\\addtolength")
                || t.starts_with("\\newcolumntype"))
        })
        .collect::<Vec<_>>()
        .join("\n");

    // Cross-references: sanitize label args (Typst labels cannot contain `'`),
    // then cleveref/autoref -> \ref (multi-key split).
    body = sanitize_label_args(&body);
    body = rewrite_ref_like(&body, "Cref");
    body = rewrite_ref_like(&body, "cref");
    body = rewrite_ref_like(&body, "autoref");
    body = rewrite_ref_like(&body, "Autoref");
    body = rewrite_crefrange(&body);
    // Citations -> \cite (natbib forms included).
    for c in [
        "autocite",
        "parencite",
        "textcite",
        "footcite",
        "citep",
        "citet",
        "citealt",
        "citealp",
    ] {
        body = rename_command_with_arg(&body, c, "cite");
    }
    // Multi-key cites -> split (typst looks up the whole list as one key).
    body = split_multicite(&body);
    // \texorpdfstring{tex}{pdf} -> tex (hyperref-only command).
    body = texorpdfstring_to_first(&body);

    // proof env -> italic lead-in.
    body = rewrite_proof(&body);
    // Strip-through envs (keep content).
    for env in [
        "minipage",
        "center",
        "quote",
        "quotation",
        "adjustbox",
        "multicols",
        "CJK*",
    ] {
        body = strip_env(&body, env);
    }
    // longtable -> tabular with plain columns.
    body = rewrite_longtable(&body);
    // tabular*/tabularx/complex tabular -> plain tabular.
    body = rewrite_tables(&body);
    // Sized delimiters without scope entries -> their plain forms.
    // (These take no braced arg, so plain name replacement.)
    for (old, new) in [
        ("bigl", "big"),
        ("bigr", "big"),
        ("Bigl", "Big"),
        ("Bigr", "Big"),
        ("biggl", "bigg"),
        ("biggr", "bigg"),
        ("Biggl", "Bigg"),
        ("Biggr", "Bigg"),
        ("bigstar", "star"),
    ] {
        body = rename_command_bare(&body, old, new);
    }
    // Figure placement options are meaningless in Typst.
    body = strip_env_options(&body, "figure");
    body = strip_env_options(&body, "table");

    // \url{X} -> X, \href{A}{B} -> B (keep last braced arg).
    body = keep_last_arg(&body, "url");
    body = keep_last_arg(&body, "href");
    body = drop_iffalse(&body);
    // `\&` renders as garbage (`amp`) through mitex; raw `&` passes through
    // fine in text (all observed uses are textual, never math alignment).
    body = body.replace("\\&", "&");
    body = body.replace("\\par", "\n\n");
    // Keep only the content of box/span-like commands.
    for cmd in [
        "multirow",
        "multicolumn",
        "uline",
        "textcolor",
        "raisebox",
        "resizebox",
        "scalebox",
        "makebox",
        "ensuremath",
    ] {
        body = keep_last_arg(&body, cmd);
    }
    body = remove_command_with_arg(&body, "color");
    body = remove_command_with_arg(&body, "tikzset");
    body = remove_command_with_arg(&body, "pgfplotsset");
    // longtable leftovers.
    for bare in ["endhead", "endfirsthead", "endfoot", "endlastfoot"] {
        body = remove_bare_command(&body, bare);
    }
    // \includesvg -> \includegraphics (asset resolution below handles the rest).
    body = body.replace("\\includesvg", "\\includegraphics");

    // ---- asset (graphics) resolution, root-relative ----
    let (body, warnings, generated) = resolve_graphics(&body, root);

    let text = format!(
        "\\documentclass{{article}}\n{kept_preamble}\\begin{{document}}\n{body}\n\\end{{document}}\n"
    );
    for w in &warnings {
        warn(w);
    }
    let mut unnumbered = Vec::new();
    if had_abstract {
        unnumbered.push((1u8, "Abstract".to_string()));
    }
    Ok(Processed {
        text,
        title: title.trim().to_string(),
        author: author.trim().to_string(),
        date: date.trim().to_string(),
        had_toc,
        unnumbered_headings: unnumbered,
        generated_assets: generated,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn fake_root() -> PathBuf {
        // Need not exist: graphics resolution only warns for missing files.
        PathBuf::from("nonexistent-root-xyz")
    }

    #[test]
    fn test_preprocess_smoke_article() {
        let bundled = "\\documentclass{article}\n\\usepackage{amsmath}\n\\title{Hi}\n\\author{A. U. Thor\\thanks{X}}\n\\begin{document}\n\\section{S}\\label{sec:s}\nText \\Cref{sec:s} and \\cite{k}.\n\\tableofcontents\n\\end{document}\n";
        let p = preprocess(bundled, &fake_root(), false).unwrap();
        assert_eq!(p.title, "Hi");
        assert_eq!(p.author, "A. U. Thor");
        assert!(p.had_toc);
        assert!(!p.text.contains("\\usepackage"), "preamble dropped: {}", p.text);
        assert!(!p.text.contains("\\tableofcontents"), "toc dropped: {}", p.text);
        // \Cref{sec:} is not a theorem label -> generic \ref survives.
        assert!(p.text.contains("\\ref{sec:s}"), "ref kept: {}", p.text);
        assert!(p.text.contains("\\cite{k}"), "cite kept: {}", p.text);
        assert!(p.unnumbered_headings.is_empty());
    }

    #[test]
    fn test_preprocess_theorems_and_abstract() {
        // NOTE: input is post-demote form (\chapter already became \section).
        let bundled = "\\documentclass{book}\n\\newtheorem{theorem}{Theorem}\n\\begin{document}\n\\section{C}\n\\begin{theorem}\\label{thm:t}Body\\end{theorem}\n\\Cref{thm:t}\n\\begin{abstract}Abs.\\end{abstract}\n\\end{document}\n";
        let p = preprocess(bundled, &fake_root(), true).unwrap();
        assert!(p.text.contains("\\textbf{Theorem 1.0.1.}"), "header: {}", p.text);
        assert!(p.text.contains("Theorem 1.0.1"), "static ref: {}", p.text);
        assert!(p.text.contains("\\newtheorem{theorem}{Theorem}"), "kept: {}", p.text);
        assert!(p.text.contains("\\section{Abstract}"), "abstract: {}", p.text);
        assert_eq!(p.unnumbered_headings, vec![(1u8, "Abstract".to_string())]);
    }
}
