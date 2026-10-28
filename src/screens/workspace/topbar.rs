//! The bar across the top of the workspace.

use super::*;
use crate::services::licence::Status;

#[allow(clippy::too_many_arguments)]
pub(super) fn topbar(
    _workspace: String,
    running: Signal<BTreeSet<Key>>,
    mut trusted: Signal<BTreeSet<Key>>,
    mut global_trust: Signal<bool>,
    mut run_view: Signal<bool>,
    mut settings_open: Signal<bool>,
    mut setup_open: Signal<bool>,
    mut mcp_open: Signal<bool>,
    licence_status: Signal<Status>,
    mut licence_open: Signal<bool>,
    mut is_light: Signal<bool>,
    mut theme_overridden: Signal<bool>,
    cfg: LlmConfig,
    props: WorkspaceProps,
) -> Element {
    rsx! {
        div { class: "topbar",
            span { class: "topbar-brand", "GitAgent" }
            div { class: "topbar-title",
                span { class: "topbar-path", "{props.workspace}" }
            }
            div { class: "topbar-right",
                button {
                    class: if *global_trust.read() { "btn btn-trusted btn-trusted-on" } else { "btn btn-ghost" },
                    title: if *global_trust.read() {
                        "Every repository and every flow is running trusted. Click to go back to approving each one yourself."
                    } else {
                        "Trust every repository and every flow: runs answer their own approvals instead of asking, the same as clicking \u{201c}Trusted run\u{201d} everywhere at once."
                    },
                    onclick: move |_| {
                        let on = !*global_trust.read();
                        global_trust.set(on);
                        store::save_trust_all(on);
                        if on {
                            trusted.write().extend(running.read().iter().cloned());
                            approvals().notify_waiters();
                        }
                    },
                    if *global_trust.read() { "Trusting everything…" } else { "Trust all" }
                }
                div { class: "view-switch",
                    button {
                        class: if *run_view.read() { "view-switch-opt" } else { "view-switch-opt view-switch-on" },
                        title: "Each step as a card, with the selected one's detail beside it",
                        onclick: move |_| run_view.set(false),
                        "List"
                    }
                    button {
                        class: if *run_view.read() { "view-switch-opt view-switch-on" } else { "view-switch-opt" },
                        title: "The selected repository's flow as a line that fills in while it runs",
                        onclick: move |_| run_view.set(true),
                        "Run"
                    }
                }
                button {
                    class: "btn btn-ghost",
                    onclick: move |_| setup_open.set(true),
                    "Setup"
                }
                button {
                    class: "btn btn-ghost",
                    title: "Use GitAgent from Claude Desktop or Claude Code",
                    onclick: move |_| mcp_open.set(true),
                    "MCP"
                }
                a {
                    class: "btn btn-ghost",
                    href: "{crate::feedback::contact_url()}",
                    target: "_blank",
                    title: "Ideas, bugs, questions: send them to the person who makes GitAgent",
                    "Feedback"
                }
                button {
                    class: "btn btn-ghost",
                    title: "The model this folder runs on \u{2014} click to change it here without touching any other window",
                    onclick: move |_| settings_open.set(true),
                    "{cfg.active_model()}"
                }
                button {
                    class: "btn btn-ghost",
                    onclick: move |_| {
                        theme_overridden.set(true);
                        let now = *is_light.read();
                        is_light.set(!now);
                    },
                    if *props.is_light.read() { "Dark" } else { "Light" }
                }
                // Last, so it sits in the corner: the one element in
                // the bar that is about you rather than the work.
                match licence_status.read().clone() {
                    licence::Status::Pro(a) => rsx! {
                        button {
                            class: "pro-badge",
                            title: "GitAgent Pro \u{b7} {a.plan.name} plan \u{b7} {a.email}",
                            onclick: move |_| licence_open.set(true),
                            span { class: "pro-badge-star", "\u{2726}" }
                            span { class: "pro-badge-text", "PRO" }
                        }
                    },
                    licence::Status::Lapsed(lapse) => rsx! {
                        button {
                            class: "pro-badge pro-badge-renew",
                            title: "{lapse.explain()}",
                            onclick: move |_| licence_open.set(true),
                            span { class: "pro-badge-star", "\u{2726}" }
                            span { class: "pro-badge-text", "PRO INACTIVE" }
                        }
                    },
                    licence::Status::Credits(a) => rsx! {
                        button {
                            class: "btn btn-ghost btn-pro",
                            title: "{a.credits_headline()}. Buy GitAgent Pro, or choose the free version's repositories",
                            onclick: move |_| licence_open.set(true),
                            "Get Pro\u{2026}"
                        }
                    },
                    licence::Status::Free => rsx! {
                        button {
                            class: "btn btn-ghost btn-pro",
                            title: "Buy GitAgent Pro, get free AI credits, paste your licence number, or choose the free version's repositories",
                            onclick: move |_| licence_open.set(true),
                            "Get Pro\u{2026}"
                        }
                    },
                }
            }
        }
    }
}
