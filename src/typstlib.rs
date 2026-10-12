//! In-process Typst compile to HTML via typst-embed (no typst binary).
//!
//! The project (entry .typ plus sibling images/bibliographies) is fed to
//! the compiler in memory. `@preview` packages resolve through the same
//! chain as typst-cli (data dir, cache dir, Universe download), so a warm
//! package cache needs no network. Fonts default to Typst's bundled set.

use std::collections::HashSet;
use std::path::PathBuf;

use typst_embed::{Project, RenderEnvironment, SyncPackageSource};

const USER_AGENT: &str = "LappTeX/0.1.0";

/// What to render: a project root on disk plus the entry document.
pub(crate) struct RenderRequest {
    /// Project root directory; sibling files are picked up from here.
    pub root: PathBuf,
    /// Root-relative entry path with forward slashes (e.g. `main.typ`).
    pub entry: String,
    /// Entry content override. `Some` uses this text (pipeline .typ built in
    /// memory); `None` reads the entry from disk (.typ-direct path).
    pub entry_text: Option<String>,
    /// Absolute paths to exclude (the output file).
    pub skip: Vec<PathBuf>,
}

/// Render an already-built project with the given packages.
fn render_project(
    project: Project,
    bundles: Vec<typst_embed::PackageBundle>,
) -> Result<String, String> {
    let env = RenderEnvironment::builder()
        .package_bundles(bundles)
        .build()
        .map_err(|e| format!("Invalid render environment: {e}"))?;
    let world = env
        .world_builder(project)
        .html()
        .build()
        .map_err(|e| format!("Invalid world: {e}"))?;
    match typst_embed::render_html_world(&world) {
        Ok(artifact) => Ok(artifact.as_str().to_string()),
        Err(diags) => Err(format_diagnostics(&diags)),
    }
}

/// Render a project to an HTML string.
pub(crate) fn render_html(req: &RenderRequest) -> Result<String, String> {
    let (project, bundles) = build_project(req)?;
    render_project(project, bundles)
}

/// Collect project files and resolve packages, without rendering.
fn build_project(
    req: &RenderRequest,
) -> Result<(Project, Vec<typst_embed::PackageBundle>), String> {
    // Collect sibling files (deterministic order).
    let mut files: Vec<PathBuf> = Vec::new();
    let mut stack = vec![req.root.clone()];
    while let Some(d) = stack.pop() {
        let entries = std::fs::read_dir(&d)
            .map_err(|e| format!("Failed to read directory {}: {e}", d.display()))?;
        for e in entries.flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else {
                files.push(p);
            }
        }
    }
    files.sort();
    let mut builder = Project::builder(req.entry.clone());
    let mut typ_texts: Vec<String> = Vec::new();
    for p in &files {
        if req.skip.iter().any(|s| s == p) {
            continue;
        }
        let rel = p
            .strip_prefix(&req.root)
            .map_err(|_| format!("File outside project root: {}", p.display()))?
            .to_string_lossy()
            .replace('\\', "/");
        if rel == req.entry {
            continue; // added separately below
        }
        if rel.ends_with(".typ") {
            match std::fs::read_to_string(p) {
                Ok(text) => {
                    typ_texts.push(text.clone());
                    builder = builder.source_file(rel, text);
                }
                Err(_) => {
                    builder = builder.file(
                        rel,
                        std::fs::read(p)
                            .map_err(|e| format!("Failed to read {}: {e}", p.display()))?,
                    );
                }
            }
        } else {
            builder = builder.file(
                rel,
                std::fs::read(p)
                    .map_err(|e| format!("Failed to read {}: {e}", p.display()))?,
            );
        }
    }
    // Entry content (in-memory override for the pipeline path).
    match &req.entry_text {
        Some(text) => {
            typ_texts.push(text.clone());
            builder = builder.source_file(req.entry.clone(), text.clone());
        }
        None => {
            let ep = req.root.join(&req.entry);
            let text = std::fs::read_to_string(&ep)
                .map_err(|e| format!("Failed to read entry {}: {e}", ep.display()))?;
            typ_texts.push(text.clone());
            builder = builder.source_file(req.entry.clone(), text);
        }
    }
    // Packages needed by any .typ source.
    let mut specs = Vec::new();
    for text in &typ_texts {
        for spec in find_package_specs(text) {
            if !specs.contains(&spec) {
                specs.push(spec);
            }
        }
    }
    let bundles = resolve_packages(&specs)?;
    let project = builder.build().map_err(|e| format!("Invalid project: {e}"))?;
    Ok((project, bundles))
}

/// Query `mitex-scope` keys of a `@preview/mitex` package import, fully
/// in-process (replaces the former `typst eval` subprocess).
pub(crate) fn query_mitex_scope(import_spec: &str) -> Result<HashSet<String>, String> {
    let probe = format!(
        "#import \"{import_spec}\": mitex-scope\n#mitex-scope.keys().join(\"|||\")\n"
    );
    let project = Project::builder("probe.typ".to_string())
        .source_file("probe.typ".to_string(), probe)
        .build()
        .map_err(|e| format!("Invalid project: {e}"))?;
    let bundles = resolve_packages(&[import_spec.to_string()])?;
    let html = render_project(project, bundles)?;
    // Strip tags; keys were joined with `|||` which survives whitespace collapse.
    let text: String = strip_tags(&html);
    Ok(text
        .split("|||")
        .map(|k| k.trim().to_string())
        .filter(|k| !k.is_empty())
        .collect())
}

fn strip_tags(html: &str) -> String {
    let mut out = String::with_capacity(html.len());
    let mut in_tag = false;
    for c in html.chars() {
        if c == '<' {
            in_tag = true;
            out.push(' ');
        } else if c == '>' {
            in_tag = false;
        } else if !in_tag {
            out.push(c);
        }
    }
    out
}

/// Resolve `@preview` specs through the CLI resolution chain
/// (data dir, cache dir, Universe download).
fn resolve_packages(specs: &[String]) -> Result<Vec<typst_embed::PackageBundle>, String> {
    let downloader = typst_embed::SystemDownloader::new(USER_AGENT);
    let source = typst_embed::SystemPackages::new(downloader);
    let mut bundles = Vec::new();
    for spec in specs {
        let parsed: typst_embed::PackageSpec =
            spec.parse().map_err(|e| format!("Bad package spec {spec}: {e}"))?;
        match source.resolve_sync(&parsed) {
            Ok(b) => bundles.push(b),
            Err(e) => {
                return Err(format!(
                    "Cannot resolve package {spec} (offline? run typst once to populate the cache): {e:?}"
                ));
            }
        }
    }
    Ok(bundles)
}

fn format_diagnostics(err: &typst_embed::RenderError) -> String {
    use typst_embed::RenderError::*;
    match err {
        Diagnostics(diags) => diags
            .iter()
            .map(|d| match d.project_path() {
                Some(p) => format!("error: {} ({})", d.message(), p.get_with_slash()),
                None => format!("error: {}", d.message()),
            })
            .collect::<Vec<_>>()
            .join("\n"),
        other => other.to_string(),
    }
}

/// Scan `@preview/name:x.y.z` specs out of Typst sources.
pub(crate) fn find_package_specs(source: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut i = 0;
    while let Some(pos) = source[i..].find("@preview/") {
        let abs = i + pos;
        let start = abs + "@preview/".len();
        let rest = &source[start..];
        let end = rest
            .find(|c: char| !(c.is_ascii_alphanumeric() || matches!(c, '/' | ':' | '.' | '-' | '_')))
            .map(|e| start + e)
            .unwrap_or(source.len());
        // Guarantee progress even on degenerate matches.
        if end <= start {
            i = abs + 1;
            continue;
        }
        let spec = source[abs..end].to_string();
        if !out.contains(&spec) {
            out.push(spec);
        }
        i = end;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_find_package_specs() {
        let src = "#import \"@preview/mitex:0.2.7\": *\n#import \"@preview/mitex:0.2.7\": foo\ntext @preview/broken";
        let specs = find_package_specs(src);
        // Dedupes; trailing text without a version still scans (resolution
        // would fail later with a clear error).
        assert_eq!(
            specs,
            vec![
                "@preview/mitex:0.2.7".to_string(),
                "@preview/broken".to_string()
            ]
        );
    }

    #[test]
    fn test_strip_tags() {
        assert_eq!(strip_tags("<p>Hi <b>x</b></p>").replace("  ", " ").trim(), "Hi x");
    }

    #[test]
    fn test_render_tiny_doc() {
        let dir = std::env::temp_dir().join(format!(
            "lapptx-test-typstlib-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let req = RenderRequest {
            root: dir.clone(),
            entry: "doc.typ".to_string(),
            entry_text: Some(
                "#set heading(numbering: \"1.1\")\n= Hi <sec:h>\nSee @sec:h.\n".to_string(),
            ),
            skip: Vec::new(),
        };
        let html = render_html(&req).expect("render tiny doc");
        assert!(html.contains("Hi"), "body: {html}");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn test_render_error_surfaces() {
        let dir = std::env::temp_dir().join(format!(
            "lapptx-test-typstlib-err-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let req = RenderRequest {
            root: dir.clone(),
            entry: "doc.typ".to_string(),
            entry_text: Some("See @sec:missing.\n".to_string()),
            skip: Vec::new(),
        };
        let err = render_html(&req).expect_err("must fail");
        assert!(err.contains("missing"), "message: {err}");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn test_query_mitex_scope() {
        // Uses the locally cached @preview/mitex package (no network).
        let keys = query_mitex_scope("@preview/mitex:0.2.7").expect("scope query");
        assert!(keys.contains("mitexcite"), "has mitexcite");
        assert!(keys.contains("frac"), "has frac");
        assert!(!keys.contains("emph"), "no emph (shadowed in prelude)");
    }
}
