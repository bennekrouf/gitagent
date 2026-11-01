//! The bar across the top of the workspace.

use super::*;
use crate::services::licence::{AccountExt, Status};
use desktop_kit::ui::{Header, ModelBadge, ProBadge, ProLook, Theme};

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
    is_light: Signal<bool>,
    theme_overridden: Signal<bool>,
    cfg: LlmConfig,
    props: WorkspaceProps,
) -> Element {
    let theme = Theme {
        light: is_light,
        picked: theme_overridden,
    };
    let (look, pro_title) = match licence_status.read().clone() {
        licence::Status::Pro(a) => (
            ProLook::Pro,
            format!("GitAgent Pro \u{b7} {} plan \u{b7} {}", a.plan.name, a.email),
        ),
        licence::Status::Lapsed(lapse) => (ProLook::Renew, lapse.explain()),
        licence::Status::Credits(a) => (
            ProLook::Get,
            format!(
                "{}. Buy GitAgent Pro, or choose the free version's repositories",
                a.credits_headline()
            ),
        ),
        licence::Status::Free => (
            ProLook::Get,
            "Buy GitAgent Pro, get free AI credits, paste your licence number, or choose the free version's repositories".to_string(),
        ),
    };
    rsx! {
        Header {
            theme,
            on_mcp: move |_| mcp_open.set(true),
            actions: rsx! {
                button {
                    class: if *global_trust.read() { "kit-btn kit-on" } else { "kit-btn" },
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
                    if *global_trust.read() { "Trusting everything\u{2026}" } else { "Trust all" }
                }
                div { class: "kit-tabs",
                    button {
                        class: if *run_view.read() { "kit-tab" } else { "kit-tab kit-on" },
                        title: "Each step as a card, with the selected one's detail beside it",
                        onclick: move |_| run_view.set(false),
                        "List"
                    }
                    button {
                        class: if *run_view.read() { "kit-tab kit-on" } else { "kit-tab" },
                        title: "The selected repository's flow as a line that fills in while it runs",
                        onclick: move |_| run_view.set(true),
                        "Run"
                    }
                }
                button { class: "kit-btn", onclick: move |_| setup_open.set(true), "Setup" }
            },
            model: rsx! {
                ModelBadge {
                    label: cfg.active_region().map_or(cfg.active_model(), |r| r.label).to_string(),
                    region: cfg.active_region().map(|r| r.id),
                    title: "The model this folder runs on \u{2014} click to change it here without touching any other window",
                    onclick: move |_| settings_open.set(true),
                }
            },
            pro: rsx! {
                ProBadge { look, title: pro_title, onclick: move |_| licence_open.set(true) }
            },
            span { class: "kit-detail", "{props.workspace}" }
        }
    }
}
