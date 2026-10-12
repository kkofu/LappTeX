//! In-process TeX→Typst conversion via the mitex library.
//!
//! Mirrors `mitex compile` from mitex-cli at the pinned rev
//! (see Cargo.toml): `mitex::convert_text` plus the alias prelude and the
//! package import line. Replaces the former `mitex` subprocess call, so no
//! mitex binary is required anymore.

use std::collections::HashSet;

use mitex_spec::{CmdShape, CommandSpecItem, EnvShape};

/// Typst package import emitted at the top of generated files.
/// Must match a published `@preview/mitex` version (see `mitex_import_spec`;
/// scope pruning reads it back dynamically).
pub(crate) const MITEX_IMPORT: &str = "#import \"@preview/mitex:0.2.7\": *";

/// Convert a bundled TeX document to a complete Typst document string.
pub(crate) fn convert_tex(tex: &str) -> Result<String, String> {
    let spec = mitex_spec_gen::DEFAULT_SPEC.clone();
    let body = mitex::convert_text(tex, Some(spec.clone())).map_err(|e| e.to_string())?;
    Ok(build_full_doc(&body, &spec))
}

fn build_full_doc(
    converted: &str,
    spec: &mitex_spec::CommandSpec,
) -> String {
    // Insert preludes (same logic as mitex-cli's `compile`).
    let mut builtin_set = HashSet::<&'static str>::new();
    builtin_set.extend(["and", "or", "in", "not", "footnote", "dot"]);
    let mut alias_set = HashSet::<Box<str>>::new();
    for (key, cmd) in spec.items() {
        let alias = match cmd {
            CommandSpecItem::Cmd(CmdShape {
                alias: Some(alias),
                ..
            }) => alias.as_str(),
            CommandSpecItem::Env(EnvShape {
                alias: Some(alias),
                ..
            }) => alias.as_str(),
            _ => key,
        };
        let alias = alias.trim_start_matches('#');
        if alias.is_empty() || !alias.chars().all(|c| c.is_ascii_alphanumeric()) {
            continue;
        }
        if builtin_set.contains(alias) || alias_set.contains(alias) {
            continue;
        }
        alias_set.insert(alias.into());
    }
    let mut alias_set = alias_set.into_iter().collect::<Vec<_>>();
    alias_set.sort();
    let preludes_str = alias_set
        .into_iter()
        .map(|alias| format!(r#"#let {alias} = mitex-scope.at("{alias}", default: none);"#))
        .collect::<Vec<_>>()
        .join("\n");
    format!("\n{MITEX_IMPORT}\n{preludes_str}\n\n{converted}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_convert_simple_doc() {
        let out = convert_tex("\\documentclass{article}\n\\begin{document}\nHi $x^2$.\n\\end{document}\n").unwrap();
        assert!(out.contains(MITEX_IMPORT), "import: {out}");
        assert!(out.contains("#let frac = mitex-scope.at(\"frac\", default: none);"), "prelude: {out}");
        assert!(out.contains("Hi"), "body: {out}");
    }

    #[test]
    fn test_convert_error_propagates() {
        // \cref is unknown to mitex (cleveref is rewritten before converting).
        assert!(convert_tex("\\begin{document}\\cref{x}\\end{document}").is_err());
    }
}
