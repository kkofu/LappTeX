use std::collections::HashSet;
use std::path::{Path, PathBuf};

use crate::tex::{is_name_char, parse_braced, read_file_lossy, strip_comments};
use crate::Args;

// ---------------------------------------------------------------------------
// Entry resolution
// ---------------------------------------------------------------------------

fn find_tex_with_documentclass(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let entries = match std::fs::read_dir(&d) {
            Ok(e) => e,
            Err(_) => continue,
        };
        for e in entries.flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else if p.extension().is_some_and(|x| x == "tex") {
                let content = read_file_lossy(&p).unwrap_or_default();
                if content.contains("\\documentclass") {
                    out.push(p);
                }
            }
        }
    }
    out.sort();
    out
}

fn depth_of(p: &Path) -> usize {
    p.components().count()
}

/// What kind of input was given: a TeX project (transpile via mitex) or a
/// Typst file (compile directly, no transpile/preprocess).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum InputKind {
    Tex,
    Typst,
}

/// Returns (project_root, entry_file, kind).
pub(crate) fn resolve_entry(args: &Args) -> Result<(PathBuf, PathBuf, InputKind), String> {
    let input = &args.input;
    if input.is_file() {
        if args.entry.is_some() {
            return Err("--entry can only be used when INPUT is a directory".to_string());
        }
        let root = input
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| PathBuf::from("."));
        let kind = if input.extension().is_some_and(|x| x == "typ") {
            InputKind::Typst
        } else {
            InputKind::Tex
        };
        return Ok((root, input.clone(), kind));
    }
    if input.is_dir() {
        let entry = match &args.entry {
            Some(e) => {
                let p = if e.is_absolute() {
                    e.clone()
                } else {
                    input.join(e)
                };
                if !p.is_file() {
                    return Err(format!("Entry file not found: {}", p.display()));
                }
                if p.extension().is_some_and(|x| x == "typ") {
                    return Err(format!(
                        "Entry is a .typ file; pass it directly instead: lapptx {}",
                        p.display()
                    ));
                }
                p
            }
            None => {
                let mut cands = find_tex_with_documentclass(input);
                if cands.is_empty() {
                    return Err(format!(
                        "No entry file (a .tex containing \\documentclass) found under {}",
                        input.display()
                    ));
                }
                cands.sort_by_key(|p| depth_of(p));
                let best = depth_of(&cands[0]);
                let top: Vec<_> = cands.iter().take_while(|p| depth_of(p) == best).collect();
                if top.len() > 1 {
                    let list = top
                        .iter()
                        .map(|p| format!("  {}", p.display()))
                        .collect::<Vec<_>>()
                        .join("\n");
                    return Err(format!(
                        "Multiple entry candidates found, specify one with --entry:\n{list}"
                    ));
                }
                cands.into_iter().next().unwrap()
            }
        };
        return Ok((input.clone(), entry, InputKind::Tex));
    }
    Err(format!("Input not found: {}", input.display()))
}

// ---------------------------------------------------------------------------
// Bundling: recursive \input / \include expansion
// ---------------------------------------------------------------------------

pub(crate) struct Bundler<'a> {
    pub(crate) root: &'a Path,
    pub(crate) visited: HashSet<PathBuf>,
    pub(crate) depth: usize,
}

fn resolve_input_file(dirs: &[PathBuf], arg: &str) -> Option<PathBuf> {
    let arg = arg.trim();
    for d in dirs {
        let p = d.join(arg);
        if p.is_file() {
            return Some(p);
        }
        let p_tex = d.join(format!("{arg}.tex"));
        if p_tex.is_file() {
            return Some(p_tex);
        }
    }
    None
}

fn canonical(p: &Path) -> PathBuf {
    std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf())
}

impl<'a> Bundler<'a> {
    pub(crate) fn bundle_file(&mut self, path: &Path) -> Result<String, String> {
        if self.depth > 50 {
            return Err(format!("\\input nesting too deep at {}", path.display()));
        }
        let canon = canonical(path);
        if !self.visited.insert(canon) {
            return Err(format!("Circular \\input detected at {}", path.display()));
        }
        let raw = read_file_lossy(path)?;
        let text = strip_comments(&raw);
        let including_dir = path
            .parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| self.root.to_path_buf());
        let dirs = vec![including_dir, self.root.to_path_buf()];

        // Scan for \input / \include (exact command names only).
        let mut out = String::with_capacity(text.len());
        let mut i = 0;
        let b = text.as_bytes();
        self.depth += 1;
        let result: Result<String, String> = (|| {
            while i < b.len() {
                let rest = &text[i..];
                let cmd = if rest.starts_with("\\input") {
                    Some("\\input")
                } else if rest.starts_with("\\include") {
                    Some("\\include")
                } else {
                    None
                };
                if let Some(c) = cmd {
                    // Make sure it is exactly this command (not \inputfoo).
                    let after = c.len();
                    let next = rest.as_bytes().get(after).copied().unwrap_or(b' ');
                    if is_name_char(next) || next == b'*' {
                        out.push_str(c);
                        i += c.len();
                        continue;
                    }
                    // Optional `*`? \input has none; be lenient and skip whitespace only.
                    match parse_braced(&text, i + c.len()) {
                        Some((arg, next_i)) => {
                            let target = resolve_input_file(&dirs, arg).ok_or_else(|| {
                                format!(
                                    "Included file `{arg}` not found (included from {})",
                                    path.display()
                                )
                            })?;
                            // Rewrite nested asset paths at expansion time is handled
                            // later globally since all paths here are root-relative
                            // (LaTeX resolves \input against the project root / cwd).
                            // To be safe we store the including dir for assets below.
                            let inner = self.bundle_file(&target)?;
                            out.push_str(&format!("\n% --- lapptx: inlined {} ---\n", target.display()));
                            out.push_str(&inner);
                            i = next_i;
                            continue;
                        }
                        None => {
                            return Err(format!(
                                "Malformed {} command near {}",
                                c,
                                path.display()
                            ));
                        }
                    }
                }
                // Copy one char (char-boundary safe).
                let ch = rest.chars().next().unwrap();
                out.push(ch);
                i += ch.len_utf8();
            }
            Ok(out)
        })();
        self.depth -= 1;
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    fn test_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "lapptx-test-{}-{}-{}",
            std::process::id(),
            name,
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write(dir: &Path, name: &str, content: &str) -> PathBuf {
        let p = dir.join(name);
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(&p, content).unwrap();
        p
    }

    fn bundler_for<'a>(root: &'a Path) -> Bundler<'a> {
        Bundler {
            root,
            visited: HashSet::new(),
            depth: 0,
        }
    }

    #[test]
    fn test_bundle_expands_nested_inputs() {
        let dir = test_dir("bundle");
        let main = write(
            &dir,
            "main.tex",
            "\\documentclass{article}\n\\begin{document}\nA\\input{sub/inner}\nB\\end{document}",
        );
        write(&dir, "sub/inner.tex", "INNER");
        let mut b = bundler_for(&dir);
        let out = b.bundle_file(&main).unwrap();
        assert!(out.contains("INNER"), "inlined: {out}");
        assert!(out.contains("\\begin{document}"), "kept: {out}");
        assert!(!out.contains("\\input"), "no leftover: {out}");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn test_bundle_missing_and_circular() {
        let dir = test_dir("bundle-err");
        let main = write(&dir, "main.tex", "A\\input{nope}B");
        let mut b = bundler_for(&dir);
        assert!(b.bundle_file(&main).is_err());
        let a = write(&dir, "a.tex", "A\\input{b}");
        write(&dir, "b.tex", "B\\input{a}");
        let mut b2 = bundler_for(&dir);
        assert!(b2.bundle_file(&a).is_err());
        std::fs::remove_dir_all(&dir).ok();
    }

    fn args_for(input: PathBuf, entry: Option<PathBuf>) -> Args {
        Args {
            input,
            output: None,
            entry,
            keep_typ: false,
            keep_bib: false,
        }
    }

    #[test]
    fn test_resolve_entry_file_and_missing() {
        let dir = test_dir("resolve");
        let main = write(&dir, "main.tex", "\\documentclass{article}");
        let (root, entry, kind) = resolve_entry(&args_for(main.clone(), None)).unwrap();
        assert_eq!(root, dir);
        assert_eq!(entry, main);
        assert_eq!(kind, InputKind::Tex);
        assert!(resolve_entry(&args_for(dir.join("nope.tex"), None)).is_err());
        assert!(resolve_entry(&args_for(main, Some(PathBuf::from("x.tex")))).is_err());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn test_resolve_entry_typ_and_dir() {
        let dir = test_dir("resolve-typ");
        let doc = write(&dir, "doc.typ", "#hello");
        let (_, _, kind) = resolve_entry(&args_for(doc, None)).unwrap();
        assert_eq!(kind, InputKind::Typst);
        write(&dir, "a.tex", "plain");
        // No documentclass anywhere -> error.
        assert!(resolve_entry(&args_for(dir.clone(), None)).is_err());
        write(&dir, "main.tex", "\\documentclass{article}");
        let (root, entry, kind) = resolve_entry(&args_for(dir.clone(), None)).unwrap();
        assert_eq!(root, dir);
        assert!(entry.ends_with("main.tex"));
        assert_eq!(kind, InputKind::Tex);
        // Multiple candidates at same depth -> error.
        write(&dir, "other.tex", "\\documentclass{book}");
        assert!(resolve_entry(&args_for(dir.clone(), None)).is_err());
        // --entry with .typ -> guidance error.
        write(&dir, "e.typ", "x");
        assert!(resolve_entry(&args_for(dir.clone(), Some(PathBuf::from("e.typ")))).is_err());
        std::fs::remove_dir_all(&dir).ok();
    }
}
