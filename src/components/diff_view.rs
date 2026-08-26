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
}

#[component]
pub fn DiffView(props: DiffViewProps) -> Element {
    let files = diffview::parse(&props.diff);

    if files.is_empty() {
        // Not a shape this parser recognises — show it raw rather than
        // silently dropping content the underlying step actually produced.
        return rsx! {
            pre { class: "log", "{props.diff}" }
        };
    }

    let theme = theme(props.is_light);

    rsx! {
        div { class: "diff-view",
            for file in files.iter() {
                {
                    let path = file.display_path().to_string();
                    let renamed = file.old_path != file.new_path
                        && file.old_path != "/dev/null"
                        && file.new_path != "/dev/null";

                    rsx! {
                        div { class: "diff-file", key: "{file.old_path}:{file.new_path}",
                            div { class: "diff-file-head",
                                if renamed {
                                    span { class: "diff-file-path", "{file.old_path} → {file.new_path}" }
                                } else {
                                    span { class: "diff-file-path", "{path}" }
                                }
                            }
                            if file.is_binary {
                                div { class: "diff-binary", "Binary file — nothing to show" }
                            } else {
                                {
                                    let syntax = guess_syntax(&path, &first_line(file));
                                    let mut old_hl = HighlightLines::new(syntax, theme);
                                    let mut new_hl = HighlightLines::new(syntax, theme);

                                    rsx! {
                                        for hunk in file.hunks.iter() {
                                            div { class: "diff-hunk", key: "{hunk.header}",
                                                div { class: "diff-hunk-head", "{hunk.header}" }
                                                for (i , line) in hunk.lines.iter().enumerate() {
                                                    {
                                                        let html = match line.kind {
                                                            LineKind::Context => {
                                                                let out = highlight(&mut old_hl, &line.text);
                                                                let _ = highlight(&mut new_hl, &line.text);
                                                                out
                                                            }
                                                            LineKind::Remove => highlight(&mut old_hl, &line.text),
                                                            LineKind::Add => highlight(&mut new_hl, &line.text),
                                                        };
                                                        rsx! {
                                                            div {
                                                                key: "{i}",
                                                                class: line_class(&line.kind),
                                                                span { class: "diff-marker", "{marker(&line.kind)}" }
                                                                span {
                                                                    class: "diff-code",
                                                                    dangerous_inner_html: "{html}",
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
                    }
                }
            }
        }
    }
}
