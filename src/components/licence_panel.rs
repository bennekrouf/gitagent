//! GitAgent Pro — the licence on this computer, and which repositories the
//! free version runs flows in.

use dioxus::prelude::*;

use crate::services::licence::{self, Account, Slots, Status, BUY_URL, FREE_REPOS};
use crate::services::store::Repo;
use crate::telemetry;

#[derive(Props, Clone, PartialEq)]
pub struct LicencePanelProps {
    pub status: Signal<Status>,
    pub slots: Signal<Slots>,
    /// For showing a slot's repository by its name rather than its path.
    pub repos: Vec<Repo>,
    /// Set when the panel opened because a run was refused: the repository
    /// that asked for a sixth slot.
    pub wanted: Option<String>,
    pub on_close: EventHandler<()>,
}

#[component]
pub fn LicencePanel(props: LicencePanelProps) -> Element {
    let mut status = props.status;
    let mut slots = props.slots;
    let mut key = use_signal(String::new);
    let mut problem = use_signal(|| Option::<String>::None);
    let close = move |_| props.on_close.call(());

    let mut busy = use_signal(|| false);

    let activate = move |_| {
        let pasted = key.peek().clone();
        busy.set(true);
        spawn(async move {
            match licence::activate(&pasted).await {
                Ok(s) => {
                    telemetry::record(telemetry::Event::LicenceActivated);
                    status.set(s);
                    key.set(String::new());
                    problem.set(None);
                }
                Err(e) => problem.set(Some(e)),
            }
            busy.set(false);
        });
    };
    let check_again = move |_| {
        busy.set(true);
        spawn(async move {
            match licence::check().await {
                Ok(s) => {
                    status.set(s);
                    problem.set(None);
                }
                Err(e) => problem.set(Some(e)),
            }
            busy.set(false);
        });
    };
    let manage = move |_| {
        busy.set(true);
        spawn(async move {
            let opened = match licence::portal_url().await {
                Ok(url) => licence::open_in_browser(&url),
                Err(e) => Err(e),
            };
            problem.set(opened.err());
            busy.set(false);
        });
    };
    let remove = move |_| {
        licence::deactivate();
        status.set(Status::Free);
        problem.set(None);
    };

    let label_of = |path: &str| {
        props
            .repos
            .iter()
            .find(|r| r.path == path)
            .map(|r| r.label.clone())
            .unwrap_or_else(|| {
                std::path::Path::new(path)
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_else(|| path.to_string())
            })
    };
    let current = status.read().clone();
    let used: Vec<(String, String)> = slots
        .read()
        .repos
        .iter()
        .map(|p| (p.clone(), label_of(p)))
        .collect();
    let wanted = props.wanted.as_deref().map(label_of);

    rsx! {
        div { class: "modal-backdrop", onclick: close,
            div {
                class: "modal",
                onclick: move |e: Event<MouseData>| e.stop_propagation(),

                div { class: "modal-head",
                    span { "GitAgent Pro" }
                    button { class: "modal-close", onclick: close, "×" }
                }

                div { class: "modal-body",
                    match &current {
                        Status::Pro(a) => rsx! {
                            p { "Licensed to " strong { "{a.email}" } ". Every repository is unlocked." }
                            PlanDetails { account: a.clone() }
                        },
                        Status::Lapsed(lapse) => rsx! {
                            p { class: "probe probe-bad", "{lapse.explain()}" }
                            if let Some(a) = lapse.account() {
                                PlanDetails { account: a.clone() }
                            }
                        },
                        Status::Free => rsx! {
                            if let Some(name) = &wanted {
                                p { class: "probe probe-bad",
                                    "{name} is locked: the free version works with {FREE_REPOS} repositories, \
                                     and they are all taken. Use it instead of one of them below, or get Pro."
                                }
                            }
                            p {
                                "The free version works with {FREE_REPOS} repositories in all, across every folder \
                                 and window. The others are listed but locked: click one to use it instead of one \
                                 of these. GitAgent Pro works with all of them, and comes with AI credits for the \
                                 api0 model each month."
                            }
                        },
                    }

                    if !current.unlimited() {
                        div { class: "field-note", "Your {FREE_REPOS} repositories" }
                        div { class: "items",
                            for (path, name) in used {
                                div { key: "{path}", class: "item",
                                    span { class: "item-label", title: "{path}", "{name}" }
                                    if let (Some(wanted_path), Some(wanted_name)) = (props.wanted.clone(), wanted.clone()) {
                                        button {
                                            class: "btn btn-ghost",
                                            title: "{wanted_name} takes this repository's place; {name} becomes locked",
                                            onclick: {
                                                // The row still shows `path`; the handler gets its own copy.
                                                let path = path.clone();
                                                move |_| {
                                                    let mut s = slots.write();
                                                    s.swap(&path, &wanted_path);
                                                    licence::save_slots(&s);
                                                    drop(s);
                                                    props.on_close.call(());
                                                }
                                            },
                                            "Use {wanted_name} instead"
                                        }
                                    }
                                }
                            }
                        }
                    }

                    if !current.unlimited() {
                        label { class: "field",
                            span { "Licence number" }
                            input {
                                spellcheck: "false",
                                placeholder: "ABCDE-FGHJK-LMNPQ-RSTUV, from your subscription email",
                                value: "{key}",
                                oninput: move |e| {
                                    problem.set(None);
                                    key.set(e.value());
                                },
                            }
                        }
                    }
                    if let Some(p) = problem.read().clone() {
                        div { class: "probe probe-bad", "{p}" }
                    }

                    div { class: "field-row",
                        if !matches!(current, Status::Free) {
                            button {
                                class: "btn btn-ghost",
                                title: "Remove the licence from this computer, e.g. to use it on another one",
                                onclick: remove,
                                "Remove from this computer"
                            }
                        }
                        if matches!(current, Status::Lapsed(_)) {
                            button {
                                class: "btn btn-ghost",
                                disabled: *busy.read(),
                                onclick: check_again,
                                "Check again"
                            }
                        }
                        if matches!(&current, Status::Pro(_)) || matches!(&current, Status::Lapsed(l) if l.account().is_some()) {
                            button {
                                class: "btn",
                                title: "Change or cancel your plan on Stripe's page, in your browser",
                                disabled: *busy.read(),
                                onclick: manage,
                                "Manage subscription\u{2026}"
                            }
                        }
                        if !current.unlimited() {
                            a { class: "btn", href: "{BUY_URL}", target: "_blank", "Buy GitAgent Pro\u{2026}" }
                            button {
                                class: "btn btn-primary",
                                disabled: key.read().trim().is_empty() || *busy.read(),
                                onclick: activate,
                                if *busy.read() { "Checking\u{2026}" } else { "Activate" }
                            }
                        }
                    }
                }
            }
        }
    }
}

/// The plan behind a licence: what it includes, the AI credits left, and when
/// it renews.
#[component]
fn PlanDetails(account: Account) -> Element {
    let renewal = account.period_end_day().map(|day| {
        if account.cancel_at_period_end {
            format!("Ends on {day}; it won't renew.")
        } else {
            format!("Renews on {day}.")
        }
    });
    let credits = &account.credits;
    rsx! {
        div { class: "field-note",
            p { "Plan: " strong { "{account.plan.name}" } }
            p { "Apps included: {account.apps()}" }
            p { "AI credits left this month: {credits.left} of {credits.granted}" }
            if let Some(r) = renewal {
                p { "{r}" }
            }
        }
    }
}
