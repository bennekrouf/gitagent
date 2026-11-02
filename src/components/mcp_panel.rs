//! How to use GitAgent from Claude: what to paste, and where.
//!
//! The command is this app's own executable, read at runtime rather than
//! written down, so the snippet is right wherever it was installed — and in a
//! development build, which is what someone trying it from a checkout runs.

use dioxus::prelude::*;

#[derive(Props, Clone, PartialEq)]
pub struct McpPanelProps {
    pub on_close: EventHandler<()>,
}

#[component]
pub fn McpPanel(props: McpPanelProps) -> Element {
    rsx! {
        desktop_kit::ui::ConnectWindow {
            lead: "Ask Claude to commit, review or release in your repositories. It runs the same flows as \
                   this window, and stops at every approval to show you exactly what it will do and ask you \
                   in the chat.",
            try_it: "\u{201c}What do my repositories need?\u{201d}, or \u{201c}Commit my changes in gitagent and \
                     open a pull request.\u{201d}",
            on_close: props.on_close,
        }
    }
}
