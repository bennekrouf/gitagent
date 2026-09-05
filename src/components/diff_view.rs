//! Renders a unified diff with per-file syntax highlighting.
//!
//! `services::diffview` turns the raw text into files/hunks/lines; this file
//! turns that into HTML. Each side of a hunk (the old file's lines, the new
//! file's lines) is highlighted as its own token stream — context lines
//! advance both, so a block comment spanning a hunk boundary still colours
//! correctly on either side, not just within one hunk.

use std::sync::OnceLock;

use dioxus::prelude::*;
use syntect::easy::HighlightLines;
use syntect::highlighting::{Theme, ThemeSet};
use syntect::html::{styled_line_to_highlighted_html, IncludeBackground};
use syntect::parsing::{SyntaxReference, SyntaxSet};

use crate::services::diffview::{self, FileDiff, LineKind};

fn syntax_set() -> &'static SyntaxSet {
    static SET: OnceLock<SyntaxSet> = OnceLock::new();
    SET.get_or_init(SyntaxSet::load_defaults_newlines)
}

fn theme_set() -> &'static ThemeSet {
    static SET: OnceLock<ThemeSet> = OnceLock::new();
    SET.get_or_init(ThemeSet::load_defaults)
}

fn theme(is_light: bool) -> &'static Theme {
    let name = if is_light {
        "InspiredGitHub"
    } else {
        "base16-ocean.dark"
    };
    &theme_set().themes[name]
}

/// Extension first, then a first-line guess (covers `Dockerfile`,
/// `Makefile`, shebang scripts) — falls back to plain text rather than
/// guessing wrong.
fn guess_syntax<'a>(path: &str, first_line: &str) -> &'a SyntaxReference {
    let set = syntax_set();
    let by_ext = std::path::Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .and_then(|ext| set.find_syntax_by_extension(ext));
    by_ext
        .or_else(|| set.find_syntax_by_first_line(first_line))
        .unwrap_or_else(|| set.find_syntax_plain_text())
}

fn highlight(hl: &mut HighlightLines, text: &str) -> String {
    let set = syntax_set();
    match hl.highlight_line(text, set) {
        Ok(ranges) => styled_line_to_highlighted_html(&ranges, IncludeBackground::No)
            .unwrap_or_else(|_| escape(text)),
        Err(_) => escape(text),
    }
}

fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn marker(kind: &LineKind) -> &'static str {
    match kind {
        LineKind::Add => "+",
        LineKind::Remove => "-",
        LineKind::Context => " ",
    }
}

fn line_class(kind: &LineKind) -> &'static str {
    match kind {
        LineKind::Add => "diff-line diff-add",
        LineKind::Remove => "diff-line diff-remove",
        LineKind::Context => "diff-line diff-context",
    }
}

/// The first line of actual content — good enough for a first-line syntax
/// guess (a shebang, an XML/HTML doctype) when the file has no extension.
fn first_line(file: &FileDiff) -> String {
    file.hunks
        .first()
        .and_then(|h| h.lines.first())
        .map(|l| l.text.clone())
        .unwrap_or_default()
}

#[derive(Props, Clone, PartialEq)]
pub struct DiffViewProps {
    pub diff: String,
    pub is_light: bool,
    /// Paths currently deselected. Everything is included by default, so an
    /// empty list means "all of it".
    #[props(default)]
    pub excluded: Vec<String>,
    /// Present only where deselecting means something. Without it the headers
    /// render exactly as before, with no checkbox at all.
    #[props(default)]
    pub on_toggle: Option<EventHandler<String>>,
}

/// A diff with the syntax highlighting already done.
///
/// Parsing and highlighting used to happen inside the component body, which
/// meant syntect re-tokenised the whole diff — up to `DIFF_CAP`, 60 KB — on
/// every single render: each keystroke, each checkbox, and once per line of
/// a streaming step's output. The work depends only on the diff text and the
/// theme, so it is done once and kept.
#[derive(Clone, PartialEq, Debug)]
struct RenderedFile {
    path: String,
    old_path: String,
    new_path: String,
    renamed: bool,
    is_binary: bool,
    hunks: Vec<RenderedHunk>,
}

#[derive(Clone, PartialEq, Debug)]
struct RenderedHunk {
    header: String,
    lines: Vec<RenderedLine>,
}

#[derive(Clone, PartialEq, Debug)]
struct RenderedLine {
    kind: LineKind,
    html: String,
}

/// Parses and highlights one diff. Pure, and the only expensive thing in
/// this module.
fn render_diff(diff: &str, is_light: bool) -> Vec<RenderedFile> {
    let theme = theme(is_light);
    diffview::parse(diff)
        .iter()
        .map(|file| {
            let path = file.display_path().to_string();
            let syntax = guess_syntax(&path, &first_line(file));
            // One token stream per side, advanced by context lines, so a
            // block comment spanning a hunk boundary still colours right.
            let mut old_hl = HighlightLines::new(syntax, theme);
            let mut new_hl = HighlightLines::new(syntax, theme);

            RenderedFile {
                renamed: file.old_path != file.new_path
                    && file.old_path != "/dev/null"
                    && file.new_path != "/dev/null",
                old_path: file.old_path.clone(),
                new_path: file.new_path.clone(),
                is_binary: file.is_binary,
                hunks: file
                    .hunks
                    .iter()
                    .map(|hunk| RenderedHunk {
                        header: hunk.header.clone(),
                        lines: hunk
                            .lines
                            .iter()
                            .map(|line| RenderedLine {
                                kind: line.kind.clone(),
                                html: match line.kind {
                                    LineKind::Context => {
                                        let out = highlight(&mut old_hl, &line.text);
                                        let _ = highlight(&mut new_hl, &line.text);
                                        out
                                    }
                                    LineKind::Remove => highlight(&mut old_hl, &line.text),
                                    LineKind::Add => highlight(&mut new_hl, &line.text),
                                },
                            })
                            .collect(),
                    })
                    .collect(),
                path,
            }
        })
        .collect()
}

#[component]
pub fn DiffView(props: DiffViewProps) -> Element {
    // Recomputed only when the diff text or the theme actually changes —
    // not on every render. `use_reactive` is what carries the props into the
    // memo's dependencies, since props are not signals.
    let diff = props.diff.clone();
    let is_light = props.is_light;
    let rendered = use_memo(use_reactive!(|diff, is_light| render_diff(&diff, is_light)));
    let files = rendered.read();

    if files.is_empty() {
        // Not a shape this parser recognises — show it raw rather than
        // silently dropping content the underlying step actually produced.
        return rsx! {
            pre { class: "log", "{props.diff}" }
        };
    }

    rsx! {
        div { class: "diff-view",
            for file in files.iter() {
                div {
                    class: if props.excluded.contains(&file.path) {
                        "diff-file diff-file-off"
                    } else {
                        "diff-file"
                    },
                    key: "{file.old_path}:{file.new_path}",
                    div { class: "diff-file-head",
                        if let Some(toggle) = props.on_toggle {
                            input {
                                class: "diff-file-check",
                                r#type: "checkbox",
                                checked: !props.excluded.contains(&file.path),
                                onchange: {
                                    let path = file.path.clone();
                                    move |_| toggle.call(path.clone())
                                },
                            }
                        }
                        if file.renamed {
                            span { class: "diff-file-path", "{file.old_path} → {file.new_path}" }
                        } else {
                            span { class: "diff-file-path", "{file.path}" }
                        }
                    }
                    if file.is_binary {
                        div { class: "diff-binary", "Binary file — nothing to show" }
                    } else {
                        for hunk in file.hunks.iter() {
                            div { class: "diff-hunk", key: "{hunk.header}",
                                div { class: "diff-hunk-head", "{hunk.header}" }
                                for (i , line) in hunk.lines.iter().enumerate() {
                                    div {
                                        key: "{i}",
                                        class: line_class(&line.kind),
                                        span { class: "diff-marker", "{marker(&line.kind)}" }
                                        span {
                                            class: "diff-code",
                                            dangerous_inner_html: "{line.html}",
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DIFF: &str = "diff --git a/src/lib.rs b/src/lib.rs\n\
        --- a/src/lib.rs\n\
        +++ b/src/lib.rs\n\
        @@ -1,3 +1,3 @@\n\
        \x20fn main() {\n\
        -    let x = 1;\n\
        +    let x = 2;\n\
        \x20}\n";

    #[test]
    fn a_diff_is_highlighted_once_into_a_renderable_shape() {
        let files = render_diff(DIFF, false);
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].path, "src/lib.rs");
        assert!(!files[0].renamed);
        assert!(!files[0].is_binary);

        let lines = &files[0].hunks[0].lines;
        assert_eq!(lines.len(), 4);
        assert_eq!(lines[1].kind, LineKind::Remove);
        assert_eq!(lines[2].kind, LineKind::Add);
        // Highlighted, not merely escaped.
        assert!(lines[2].html.contains("<span"), "got {:?}", lines[2].html);
        // Tokenised, so the text is split across spans rather than intact.
        assert!(lines[2].html.contains("let"), "got {:?}", lines[2].html);
        assert!(lines[2].html.contains('2'), "got {:?}", lines[2].html);
    }

    #[test]
    fn rendering_is_deterministic_so_the_memo_can_hold_it() {
        // `use_memo` only skips work when equal inputs compare equal out.
        assert_eq!(render_diff(DIFF, false), render_diff(DIFF, false));
        assert_ne!(
            render_diff(DIFF, true),
            render_diff(DIFF, false),
            "the theme is part of the result, so it must be a dependency"
        );
    }

    #[test]
    fn a_rename_is_flagged_and_a_binary_file_has_no_hunks() {
        let renamed = render_diff(
            "diff --git a/old.rs b/new.rs\n--- a/old.rs\n+++ b/new.rs\n",
            false,
        );
        assert!(renamed[0].renamed);

        let binary = render_diff(
            "diff --git a/logo.png b/logo.png\nBinary files a/logo.png and b/logo.png differ\n",
            false,
        );
        assert!(binary[0].is_binary);
        assert!(binary[0].hunks.is_empty());
    }
}
