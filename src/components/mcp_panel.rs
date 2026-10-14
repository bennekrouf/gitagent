//! How to use GitAgent from Claude: what to paste, and where.
//!
//! The command is this app's own executable, read at runtime rather than
//! written down, so the snippet is right wherever it was installed — and in a
//! development build, which is what someone trying it from a checkout runs.

use dioxus::prelude::*;

use crate::components::detail_pane::CopyButton;

#[derive(Props, Clone, PartialEq)]
pub struct McpPanelProps {
    pub on_close: EventHandler<()>,
}

/// This executable, as a client should start it.
fn command() -> String {
    std::env::current_exe()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|_| "gitagent".into())
}

/// The `mcpServers` entry for Claude Desktop's config file. Built with serde
/// so a Windows path's backslashes arrive escaped.
pub fn desktop_config(command: &str) -> String {
    let config = serde_json::json!({
        "mcpServers": { "gitagent": { "command": command, "args": ["mcp"] } }
    });
    serde_json::to_string_pretty(&config).unwrap_or_default()
}

/// The one line that adds it to Claude Code, for every project.
pub fn code_command(command: &str) -> String {
    format!("claude mcp add gitagent --scope user -- \"{command}\" mcp")
}

/// Where Claude Desktop keeps its config, for the person who would rather
/// open it themselves than go through the app's menu.
fn desktop_config_path() -> &'static str {
    if cfg!(target_os = "windows") {
        "%APPDATA%\\Claude\\claude_desktop_config.json"
    } else if cfg!(target_os = "macos") {
        "~/Library/Application Support/Claude/claude_desktop_config.json"
    } else {
        "~/.config/Claude/claude_desktop_config.json"
    }
}

#[component]
pub fn McpPanel(props: McpPanelProps) -> Element {
    let command = command();
    let desktop = desktop_config(&command);
    let code = code_command(&command);
    let close = move |_| props.on_close.call(());

    rsx! {
        div { class: "modal-backdrop", onclick: close,
            div {
                class: "modal modal-wide",
                onclick: move |e: Event<MouseData>| e.stop_propagation(),
                div { class: "modal-head",
                    span { "Use GitAgent from Claude" }
                    button { class: "modal-close", onclick: close, "×" }
                }
                div { class: "modal-body mcp-body",
                    p { class: "mcp-lead",
                        "Ask Claude to commit, review or release in your repositories. It runs \
                         the same flows as this window, and stops at every approval to show you \
                         exactly what it will do and ask you in the chat."
                    }

                    div { class: "mcp-step",
                        div { class: "mcp-step-title", span { class: "mcp-num", "1" } "Claude Desktop" }
                        p { class: "field-note",
                            "Settings \u{2192} Developer \u{2192} Edit Config, paste this in and save, \
                             then quit Claude and open it again. If the file already has an "
                            code { "mcpServers" }
                            " section, add just the "
                            code { "gitagent" }
                            " entry to it."
                        }
                        div { class: "log-wrap",
                            pre { class: "log mcp-snippet", "{desktop}" }
                            CopyButton { text: desktop.clone() }
                        }
                        p { class: "field-note mcp-path", "The file: {desktop_config_path()}" }
                    }

                    div { class: "mcp-step",
                        div { class: "mcp-step-title", span { class: "mcp-num", "2" } "Claude Code" }
                        p { class: "field-note", "Run this once in a terminal:" }
                        div { class: "log-wrap",
                            pre { class: "log mcp-snippet", "{code}" }
                            CopyButton { text: code.clone() }
                        }
                    }

                    div { class: "mcp-step",
                        div { class: "mcp-step-title", span { class: "mcp-num", "3" } "Try it" }
                        p { class: "field-note",
                            "Ask: \u{201c}What do my repositories need?\u{201d}, or \u{201c}Commit my \
                             changes in gitagent and open a pull request.\u{201d}"
                        }
                    }

                    p { class: "mcp-note",
                        "Claude in the browser (claude.ai) can\u{2019}t use this: it only connects to \
                         servers on the internet, and GitAgent runs on your computer. Any other app \
                         that speaks MCP can, with the same command: "
                        code { "{command} mcp" }
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_desktop_entry_starts_this_app_in_mcp_mode() {
        let config: serde_json::Value = serde_json::from_str(&desktop_config(
            "/Applications/GitAgent.app/Contents/MacOS/gitagent",
        ))
        .unwrap();
        let entry = &config["mcpServers"]["gitagent"];
        assert_eq!(
            entry["command"],
            "/Applications/GitAgent.app/Contents/MacOS/gitagent"
        );
        assert_eq!(entry["args"], serde_json::json!(["mcp"]));
    }

    #[test]
    fn a_windows_path_survives_the_trip_through_json() {
        let path = r"C:\Program Files\GitAgent\gitagent.exe";
        let config: serde_json::Value = serde_json::from_str(&desktop_config(path)).unwrap();
        assert_eq!(config["mcpServers"]["gitagent"]["command"], path);
    }

    #[test]
    fn the_code_command_quotes_a_path_with_spaces() {
        assert_eq!(
            code_command("/Apps/Git Agent/gitagent"),
            "claude mcp add gitagent --scope user -- \"/Apps/Git Agent/gitagent\" mcp"
        );
    }
}
