use std::path::{Path, PathBuf};

use crate::cmd::run;
use crate::tex::{parse_braced, rewrite_cmd, skip_star_brackets, CmdEdit};

const READABLE_IMAGE_EXTS: [&str; 5] = ["svg", "png", "jpg", "jpeg", "gif"];

/// Resolve `\includegraphics` paths (root-relative). PDFs without a readable
/// twin are converted via poppler (`pdftocairo` preferred, `pdftoppm`
/// fallback); files that cannot be made readable are dropped.
/// Returns (text, warnings, generated_files_for_cleanup).
pub(crate) fn resolve_graphics(
    body: &str,
    root: &Path,
) -> (String, Vec<String>, Vec<PathBuf>) {
    if !body.contains("\\includegraphics") {
        return (body.to_string(), Vec::new(), Vec::new());
    }
    let mut warnings = Vec::new();
    let mut generated = Vec::new();
    let out = rewrite_cmd(body, "includegraphics", |h| {
        let j = skip_star_brackets(h.body, h.after);
        match parse_braced(h.body, j) {
            Some((arg, next_i)) => {
                match resolve_graphic_path(root, arg.trim(), &mut warnings, &mut generated) {
                    Some(new_path) => {
                        // Re-emit without options (mitex handles plain form best).
                        CmdEdit::Replace(format!("\\includegraphics{{{new_path}}}"), next_i)
                    }
                    None => {
                        // Unreadable figure: drop the command, keep the caption.
                        warnings.push(format!("dropping unreadable figure: {}", arg.trim()));
                        CmdEdit::Replace(String::new(), next_i)
                    }
                }
            }
            None => CmdEdit::Keep,
        }
    });
    (out, warnings, generated)
}

/// Poppler converter preference: SVG (vector) first, PNG fallback.
enum Converter {
    Svg(String),
    Png(String),
}

fn pick_converter() -> Option<Converter> {
    if let Some(bin) = find_on_path("pdftocairo") {
        return Some(Converter::Svg(bin));
    }
    if let Some(bin) = find_on_path("pdftoppm") {
        return Some(Converter::Png(bin));
    }
    None
}

fn find_on_path(name: &str) -> Option<String> {
    let exe = if cfg!(windows) {
        format!("{name}.exe")
    } else {
        name.to_string()
    };
    for dir in std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()) {
        let cand = dir.join(&exe);
        if cand.is_file() {
            return Some(cand.to_string_lossy().into_owned());
        }
        // Also try without extension (Unix-style install on Windows PATH).
        let cand2 = dir.join(name);
        if cand2.is_file() {
            return Some(cand2.to_string_lossy().into_owned());
        }
    }
    None
}

/// Convert the first page of a PDF figure. Returns the generated file path.
fn convert_pdf(pdf: &Path, converter: &Converter) -> Option<PathBuf> {
    let stem = pdf.file_stem()?.to_string_lossy().into_owned();
    let dir = pdf.parent()?;
    match converter {
        Converter::Svg(bin) => {
            let out = dir.join(format!("{stem}.lapptx.svg"));
            run(
                bin,
                &[
                    "-svg",
                    "-f",
                    "1",
                    "-l",
                    "1",
                    &pdf.to_string_lossy(),
                    &out.to_string_lossy(),
                ],
            )
            .ok()?;
            out.is_file().then_some(out)
        }
        Converter::Png(bin) => {
            let prefix = dir.join(format!("{stem}.lapptx"));
            run(
                bin,
                &[
                    "-png",
                    "-r",
                    "150",
                    "-f",
                    "1",
                    "-l",
                    "1",
                    &pdf.to_string_lossy(),
                    &prefix.to_string_lossy(),
                ],
            )
            .ok()?;
            // pdftoppm names output <prefix>-1.png.
            let out = dir.join(format!("{stem}.lapptx-1.png"));
            out.is_file().then_some(out)
        }
    }
}

fn resolve_graphic_path(
    root: &Path,
    arg: &str,
    warnings: &mut Vec<String>,
    generated: &mut Vec<PathBuf>,
) -> Option<String> {
    let arg_path = Path::new(arg);
    let full = root.join(arg_path);
    if full.is_file() {
        let ext = arg_path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        if matches!(ext.as_str(), "pdf" | "eps" | "ps") {
            let stem = full.with_extension("");
            for e in READABLE_IMAGE_EXTS {
                let twin = stem.with_extension(e);
                if twin.is_file() {
                    let rel = twin
                        .strip_prefix(root)
                        .unwrap_or(&twin)
                        .to_string_lossy()
                        .replace('\\', "/");
                    warnings.push(format!(
                        "{arg} -> {rel} (Typst cannot embed PDF/EPS images)"
                    ));
                    return Some(rel);
                }
            }
            // No readable twin: convert the PDF (first page) if possible.
            match pick_converter() {
                Some(conv) => match convert_pdf(&full, &conv) {
                    Some(out) => {
                        let rel = out
                            .strip_prefix(root)
                            .unwrap_or(&out)
                            .to_string_lossy()
                            .replace('\\', "/");
                        warnings.push(format!("{arg} -> {rel} (converted first page to SVG/PNG)"));
                        generated.push(out);
                        return Some(rel);
                    }
                    None => {
                        warnings.push(format!("{arg}: PDF conversion failed"));
                        return None;
                    }
                },
                None => {
                    warnings.push(format!(
                        "{arg} is PDF/EPS which Typst cannot embed (no twin, no converter found)"
                    ));
                    return None;
                }
            }
        }
        return Some(arg.replace('\\', "/"));
    }
    // Try adding readable extensions (for extension-less references).
    for e in READABLE_IMAGE_EXTS {
        let cand = root.join(format!("{arg}.{e}"));
        if cand.is_file() {
            let rel = cand
                .strip_prefix(root)
                .unwrap_or(&cand)
                .to_string_lossy()
                .replace('\\', "/");
            return Some(rel);
        }
    }
    warnings.push(format!("Figure file not found: {arg}"));
    Some(arg.replace('\\', "/"))
}

#[cfg(test)]
mod tests {
    use super::*;

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

    #[test]
    fn test_resolve_readable_and_twin() {
        let dir = test_dir("gfx");
        std::fs::write(dir.join("a.png"), "fake").unwrap();
        std::fs::write(dir.join("b.pdf"), "fake").unwrap();
        std::fs::write(dir.join("b.svg"), "fake").unwrap();
        let (out, warns, generated) = resolve_graphics(
            "A\\includegraphics{a.png}B\\includegraphics{b.pdf}C",
            &dir,
        );
        assert!(out.contains("\\includegraphics{a.png}"), "kept: {out}");
        assert!(out.contains("\\includegraphics{b.svg}"), "twin: {out}");
        assert!(generated.is_empty(), "no conversion needed");
        assert_eq!(warns.len(), 1, "one twin warning: {warns:?}");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn test_resolve_missing_kept_with_warning() {
        let dir = test_dir("gfx-missing");
        let (out, warns, _) =
            resolve_graphics("A\\includegraphics{figures/nope}C", &dir);
        // Missing files are kept (typst reports the path) with a warning.
        assert!(out.contains("\\includegraphics{figures/nope}"), "kept: {out}");
        assert_eq!(warns.len(), 1);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn test_resolve_unreadable_pdf_dropped() {
        let dir = test_dir("gfx-pdf");
        // Fake PDF: no readable twin; conversion fails deterministically.
        std::fs::write(dir.join("x.pdf"), "not a pdf").unwrap();
        let (out, warns, _) =
            resolve_graphics("A\\includegraphics[width=2cm]{x.pdf}C", &dir);
        assert!(!out.contains("includegraphics"), "dropped: {out}");
        assert_eq!(out, "AC");
        assert!(!warns.is_empty());
        std::fs::remove_dir_all(&dir).ok();
    }
}
