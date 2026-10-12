mod bib;
mod bundle;
mod cmd;
mod envs;
mod graphics;
mod headings;
mod macros;
mod mitexlib;
mod preprocess;
mod refs;
mod tex;
mod theorems;
mod typst;
mod typstlib;

use std::collections::HashSet;
use std::env;
use std::ffi::OsString;
use std::path::PathBuf;

use bib::{collect_cite_keys, find_existing_bib, first_bbl, synth_bib_from_bbl};
use bundle::{Bundler, InputKind, resolve_entry};
use cmd::warn;
use headings::{demote_headings, resolve_para_refs};
use macros::{expand_macros, extract_macros};
use mitexlib::convert_tex;
use preprocess::preprocess;
use refs::strip_ref_prefix;
use tex::read_file_lossy;
use typst::{mitex_import_spec, postprocess_typ, prune_shadowed_prelude};
use typstlib::{render_html, RenderRequest};

// ---------------------------------------------------------------------------
// CLI
// ---------------------------------------------------------------------------

fn usage() -> String {
    format!(
        "Usage: {} <INPUT.tex | INPUT_DIR | INPUT.typ> [OUTPUT.html | -o OUTPUT.html] [OPTIONS]\n\
         \n\
         Converts a (possibly multi-file) TeX project to Typst via the embedded\n\
         `mitex` library and compiles it to HTML in-process.\n\
         A .typ file input is compiled directly (no transpile/postprocess).\n\
         \n\
         Args:\n  \
         <INPUT>  Path to the entry .tex/.typ file or to a project directory (required)\n  \
         [OUTPUT.html]  Path to the output HTML file\n  \
         (default: entry file with .html extension)\n\
         \n\
         Options:\n  \
         -o, --output <OUTPUT.html>  Same as the positional output argument\n  \
         --entry <MAIN.tex>          Entry file when INPUT is a directory\n  \
         (auto-detected if omitted)\n  \
         --keep-typ                 Keep the intermediate .typ file\n  \
         --keep-bib                 Keep the synthesized .bib file\n  \
         -h, --help                 Show this help\n\
         \n\
         Env:\n  \
         (no external binaries required for TeX input; PDF figures need\n  \
         `pdftocairo` or `pdftoppm` on PATH, else they are dropped)",
        env::args()
            .next()
            .unwrap_or_else(|| "lapptx".to_string())
    )
}

pub(crate) struct Args {
    pub(crate) input: PathBuf,
    pub(crate) output: Option<PathBuf>,
    pub(crate) entry: Option<PathBuf>,
    pub(crate) keep_typ: bool,
    pub(crate) keep_bib: bool,
}

fn parse_args() -> Result<Args, String> {
    parse_args_from(&env::args_os().skip(1).collect::<Vec<_>>())
}

/// Parse CLI args from a slice (split out for testability; `--help` still
/// exits the process and is therefore not unit-tested).
fn parse_args_from(raw: &[OsString]) -> Result<Args, String> {
    let mut input: Option<PathBuf> = None;
    let mut output: Option<PathBuf> = None;
    let mut entry: Option<PathBuf> = None;
    let mut keep_typ = false;
    let mut keep_bib = false;
    let mut i = 0;
    while i < raw.len() {
        let arg = raw[i].to_string_lossy().into_owned();
        match arg.as_str() {
            "-h" | "--help" => {
                println!("{}", usage());
                std::process::exit(0);
            }
            "-o" | "--output" => {
                i += 1;
                let val = raw.get(i).ok_or_else(|| "Missing value for -o/--output".to_string())?;
                output = Some(PathBuf::from(val));
            }
            "--entry" => {
                i += 1;
                let val = raw.get(i).ok_or_else(|| "Missing value for --entry".to_string())?;
                entry = Some(PathBuf::from(val));
            }
            "--keep-typ" => keep_typ = true,
            "--keep-bib" => keep_bib = true,
            _ if arg.starts_with('-') => {
                return Err(format!("Unknown option: {arg}\n\n{}", usage()));
            }
            _ => {
                if input.is_none() {
                    input = Some(PathBuf::from(&raw[i]));
                } else if output.is_none() {
                    output = Some(PathBuf::from(&raw[i]));
                } else {
                    return Err(format!("Too many arguments\n\n{}", usage()));
                }
            }
        }
        i += 1;
    }
    let input = input.ok_or_else(usage)?;
    Ok(Args {
        input,
        output,
        entry,
        keep_typ,
        keep_bib,
    })
}

// ---------------------------------------------------------------------------
// Main pipeline
// ---------------------------------------------------------------------------

fn convert(args: &Args) -> Result<(), String> {
    let (root, entry, kind) = resolve_entry(args)?;
    let output = match &args.output {
        Some(o) => o.clone(),
        None => entry.with_extension("html"),
    };
    if let Some(parent) = output.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("Failed to create output directory {}: {e}", parent.display()))?;
    }

    // .typ input: compile directly, no transpile/preprocess/postprocess.
    if kind == InputKind::Typst {
        if args.keep_typ || args.keep_bib {
            eprintln!("note: --keep-typ/--keep-bib have no effect for .typ input");
        }
        let entry_rel = entry
            .strip_prefix(&root)
            .map_err(|_| format!("Entry outside project root: {}", entry.display()))?
            .to_string_lossy()
            .replace('\\', "/");
        let html = render_html(&RenderRequest {
            root: root.clone(),
            entry: entry_rel,
            entry_text: None,
            skip: vec![output.clone()],
        })
        .map_err(|e| format!("typst step failed: {e}"))?;
        std::fs::write(&output, html)
            .map_err(|e| format!("Failed to write {}: {e}", output.display()))?;
        return Ok(());
    }

    // 1. Bundle.
    let mut bundler = Bundler {
        root: &root,
        visited: HashSet::new(),
        depth: 0,
    };
    let bundled = bundler.bundle_file(&entry)?;

    // 1b. Expand \newcommand-style macros textually (mitex only knows its
    // builtin set). Definitions are collected from the whole bundle and
    // removed; call sites are substituted (fixpoint).
    let (bundled, macros) = extract_macros(&bundled);
    let mut bundled = if macros.is_empty() {
        bundled
    } else {
        eprintln!("expanding {} macro(s)", macros.len());
        expand_macros(&bundled, &macros)
    };

    // 1c. Drop manual ref prefixes (Fig./section/\S): Typst renders native
    // supplements itself. Must run before paragraph refs go static (which
    // would orphan the prefixes).
    bundled = strip_ref_prefix(&bundled);

    // 1d. Paragraph-title references need raw \paragraph commands.
    bundled = resolve_para_refs(&bundled);

    // 2. Preprocess: heading demotion (single pass, no cascade) first,
    //    then the rest. Starred headings are recorded for unnumbered rendering.
    let (bundled, star_headings, has_chapters) = demote_headings(&bundled);
    let mut processed = preprocess(&bundled, &root, has_chapters)?;
    processed.unnumbered_headings.extend(star_headings);

    // 3. Citations + bibliography.
    let cite_keys = collect_cite_keys(&processed.text);
    let bib_name: Option<String> = if cite_keys.is_empty() {
        None
    } else {
        // Prefer an existing .bib if the project already has the referenced one.
        let existing = find_existing_bib(&root, &cite_keys);
        match existing {
            Some(name) => {
                eprintln!("using existing bibliography: {name}");
                Some(name)
            }
            None => {
                let bbl_path = root.join(entry.file_stem().unwrap()).with_extension("bbl");
                let bbl_path = if bbl_path.is_file() {
                    Some(bbl_path)
                } else {
                    first_bbl(&root)
                };
                let bib_text = match bbl_path {
                    Some(p) => {
                        let bbl = read_file_lossy(&p)?;
                        let (bib, missing) = synth_bib_from_bbl(&bbl, &cite_keys);
                        for m in &missing {
                            warn(&format!("No bbl entry for `{m}`, using stub entry"));
                        }
                        bib
                    }
                    None => {
                        warn("No .bbl found, using stub bibliography entries");
                        cite_keys
                            .iter()
                            .map(|k| {
                                format!("@misc{{{k},\n  author = {{Unknown}},\n  title = {{{k}}}\n}}\n\n")
                            })
                            .collect()
                    }
                };
                let name = format!(".lapptx-{}-refs.bib", std::process::id());
                std::fs::write(root.join(&name), bib_text).map_err(|e| {
                    format!("Failed to write synthesized bibliography: {e}")
                })?;
                Some(name)
            }
        }
    };

    // 4. Convert in-process via the embedded mitex library, writing the
    //    intermediate .typ inside the project root (so that relative
    //    image/bib paths resolve for the typst step).
    let tag = format!("{}-{}", std::process::id(), unique());
    let bundle_tmp = root.join(format!(".lapptx-{tag}-bundle.tex"));
    let typ_path = root.join(format!(".lapptx-{tag}.typ"));

    let bib_path = bib_name
        .as_ref()
        .filter(|n| n.starts_with(".lapptx-"))
        .map(|n| root.join(n));

    let result = (|| -> Result<(), String> {
        let typ = convert_tex(&processed.text)
            .map_err(|e| format!("mitex step failed: {e}"))?;
        let spec = mitex_import_spec(&typ).unwrap_or_else(|| "@preview/mitex:0.2.7".to_string());
        let (typ, scope_keys) = prune_shadowed_prelude(&typ, &spec)?;
        let typ = postprocess_typ(&typ, &processed, bib_name.as_deref(), &scope_keys);
        // Persist the intermediate .typ (removed on success unless kept).
        std::fs::write(&typ_path, &typ)
            .map_err(|e| format!("Failed to write intermediate .typ: {e}"))?;
        let entry_name = typ_path
            .file_name()
            .map(|n| n.to_string_lossy().replace('\\', "/"))
            .unwrap_or_else(|| "main.typ".to_string());
        let html = render_html(&RenderRequest {
            root: root.clone(),
            entry: entry_name,
            entry_text: Some(typ),
            skip: vec![output.clone()],
        })
        .map_err(|e| format!("typst step failed: {e}"))?;
        std::fs::write(&output, html)
            .map_err(|e| format!("Failed to write {}: {e}", output.display()))?;
        Ok(())
    })();

    match &result {
        Ok(_) => {
            for p in &processed.generated_assets {
                let _ = std::fs::remove_file(p);
            }
            if !args.keep_typ {
                let _ = std::fs::remove_file(&typ_path);
            } else {
                eprintln!("kept intermediate: {}", typ_path.display());
            }
            if let Some(bp) = &bib_path {
                if args.keep_bib {
                    eprintln!("kept bibliography: {}", bp.display());
                } else {
                    let _ = std::fs::remove_file(bp);
                }
            }
        }
        Err(_) => {
            // Keep intermediates for debugging (bundle is only written here,
            // so success leaves nothing behind).
            if std::fs::write(&bundle_tmp, &processed.text).is_ok() {
                eprintln!("kept bundle: {}", bundle_tmp.display());
            }
            if typ_path.is_file() {
                eprintln!("kept intermediate: {}", typ_path.display());
            }
            for p in &processed.generated_assets {
                eprintln!("kept converted figure: {}", p.display());
            }
            if let Some(bp) = &bib_path {
                eprintln!("kept bibliography: {}", bp.display());
            }
        }
    }
    result
}

/// Cheap unique suffix without external crates (nanos since epoch).
fn unique() -> u128 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos()
}

fn main() {
    let args = match parse_args() {
        Ok(v) => v,
        Err(msg) => {
            eprintln!("{msg}");
            std::process::exit(2);
        }
    };

    match convert(&args) {
        Ok(_) => println!("done"),
        Err(e) => {
            eprintln!("Error: {e}");
            std::process::exit(1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn os(args: &[&str]) -> Vec<OsString> {
        args.iter().map(OsString::from).collect()
    }

    #[test]
    fn test_parse_positional_and_flags() {
        let a = parse_args_from(&os(&["in.tex", "out.html"])).unwrap();
        assert_eq!(a.input, PathBuf::from("in.tex"));
        assert_eq!(a.output, Some(PathBuf::from("out.html")));
        assert!(!a.keep_typ && !a.keep_bib);
        let b = parse_args_from(&os(&["in.tex", "-o", "o.html", "--entry", "m.tex", "--keep-typ", "--keep-bib"])).unwrap();
        assert_eq!(b.output, Some(PathBuf::from("o.html")));
        assert_eq!(b.entry, Some(PathBuf::from("m.tex")));
        assert!(b.keep_typ && b.keep_bib);
    }

    #[test]
    fn test_parse_errors() {
        assert!(parse_args_from(&os(&[])).is_err());
        assert!(parse_args_from(&os(&["a", "b", "c"])).is_err());
        assert!(parse_args_from(&os(&["--bogus"])).is_err());
        assert!(parse_args_from(&os(&["-o"])).is_err());
    }
}
