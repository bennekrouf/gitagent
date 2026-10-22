//! GitAgent Pro — the licence on this computer: its plan, its AI credits, and
//! activating, checking or removing it.

use dioxus::prelude::*;

use crate::services::licence::{self, Account, FreeReply, Status, BUY_URL};
use crate::telemetry;

#[derive(Props, Clone, PartialEq)]
pub struct LicencePanelProps {
    pub status: Signal<Status>,
    pub on_close: EventHandler<()>,
}

#[component]
pub fn LicencePanel(props: LicencePanelProps) -> Element {
    let mut status = props.status;
    let mut key = use_signal(String::new);
    let mut problem = use_signal(|| Option::<String>::None);
    let close = move |_| props.on_close.call(());

    let mut busy = use_signal(|| false);
    // Set by an Activate that worked: the window then says so and nothing
    // else. Removing or managing the licence you have just pasted is the last
    // thing to put in front of someone at that moment.
    let mut activated = use_signal(|| false);
    let mut email = use_signal(String::new);
    // What asking for the free licence came to, once asked.
    let mut free_reply = use_signal(|| Option::<FreeReply>::None);

    let ask_free = move |_| {
        let to = email.peek().clone();
        busy.set(true);
        spawn(async move {
            free_reply.set(Some(licence::request_free_credits(&to).await));
            busy.set(false);
        });
    };

    let activate = move |_| {
        let pasted = key.peek().clone();
        busy.set(true);
        spawn(async move {
            match licence::activate(&pasted).await {
                Ok(s) => {
                    telemetry::record(telemetry::Event::LicenceActivated);
                    // A saved licence that doesn't work now (a cancelled
                    // plan, say) is not a success: the window shows why.
                    activated.set(matches!(s, Status::Pro(_) | Status::Credits(_)));
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

    let current = status.read().clone();

    if *activated.read() {
        let (title, detail) = match &current {
            Status::Pro(a) => (
                "GitAgent Pro is active.".to_string(),
                format!(
                    "Licensed to {}, with {} AI credits this month.",
                    a.email, a.credits.left
                ),
            ),
            Status::Credits(a) => (
                "Licence saved.".to_string(),
                format!("{} for {}.", a.credits_headline(), a.email),
            ),
            _ => (String::new(), String::new()),
        };
        return rsx! {
            div { class: "modal-backdrop", onclick: close,
                div {
                    class: "modal",
                    onclick: move |e: Event<MouseData>| e.stop_propagation(),
                    div { class: "modal-head",
                        span { "GitAgent Pro" }
                        button { class: "modal-close", onclick: close, "×" }
                    }
                    div { class: "modal-body",
                        div { class: "licence-done",
                            span { class: "licence-done-mark", "\u{2713}" }
                            div {
                                div { class: "licence-done-title", "{title}" }
                                div { class: "field-note", "{detail}" }
                            }
                        }
                        div { class: "licence-actions",
                            span { class: "licence-actions-gap" }
                            button { class: "btn btn-primary", onclick: close, "Done" }
                        }
                    }
                }
            }
        };
    }

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
                            p { "Licensed to " strong { "{a.email}" } "." }
                            PlanDetails { account: a.clone() }
                        },
                        Status::Lapsed(lapse) => rsx! {
                            p { class: "probe probe-bad", "{lapse.explain()}" }
                            if let Some(a) = lapse.account() {
                                PlanDetails { account: a.clone() }
                            }
                        },
                        Status::Credits(a) => rsx! {
                            p { strong { "{a.credits_headline()}" } ", for the api0 model." }
                            if a.is_free() {
                                p { class: "field-note",
                                    "The free licence of {a.email} gives AI credits each month. Buy a plan with \
                                     the same email and this licence moves to it, with nothing to paste again."
                                }
                            } else {
                                p { class: "field-note",
                                    "The licence of {a.email} ({a.plan.name}) gives AI credits for the api0 \
                                     model, but its plan doesn't include GitAgent Pro."
                                }
                                PlanDetails { account: a.clone() }
                            }
                        },
                        Status::Free => rsx! {
                            p {
                                "GitAgent works in every repository, with or without a licence. A licence \
                                 adds AI credits each month for the api0 model: a few with the free one, \
                                 more with GitAgent Pro."
                            }
                        },
                    }

                    if !current.is_pro() {
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

                    if matches!(current, Status::Free | Status::Lapsed(_)) {
                        if *free_reply.read() == Some(FreeReply::Unavailable) {
                            div { class: "field-note", "There are no free AI credits right now." }
                        } else {
                            div { class: "field-note",
                                "No licence yet? Get free AI credits for the api0 model each month: the licence \
                                 number is sent to your email."
                            }
                            div { class: "field-row",
                                input {
                                    r#type: "email",
                                    spellcheck: "false",
                                    placeholder: "you@example.com",
                                    value: "{email}",
                                    oninput: move |e| {
                                        free_reply.set(None);
                                        email.set(e.value());
                                    },
                                }
                                button {
                                    class: "btn",
                                    disabled: email.read().trim().is_empty() || *busy.read(),
                                    onclick: ask_free,
                                    "Get free AI credits"
                                }
                            }
                            match free_reply.read().clone() {
                                Some(FreeReply::Sent(m)) => rsx! { div { class: "probe probe-ok", "{m}" } },
                                Some(FreeReply::Refused(m)) => rsx! { div { class: "probe probe-bad", "{m}" } },
                                _ => rsx! {},
                            }
                        }
                    }

                    div { class: "licence-actions",
                        if !matches!(current, Status::Free) {
                            button {
                                class: "btn btn-ghost",
                                title: "Removes the licence from every Mayorana app on this computer, e.g. to use it on another one",
                                onclick: remove,
                                "Remove from this computer"
                            }
                        }
                        if matches!(current, Status::Lapsed(_) | Status::Credits(_)) {
                            button {
                                class: "btn btn-ghost",
                                title: "Ask api0 again what this licence unlocks, e.g. after subscribing",
                                disabled: *busy.read(),
                                onclick: check_again,
                                "Check again"
                            }
                        }
                        // Anyone paying can reach their subscription, whatever
                        // GitAgent makes of the plan.
                        if matches!(&current, Status::Pro(_))
                            || matches!(&current, Status::Credits(a) if !a.is_free())
                            || matches!(&current, Status::Lapsed(l) if l.account().is_some())
                        {
                            button {
                                class: "btn",
                                title: "Change or cancel your plan on Stripe's page, in your browser",
                                disabled: *busy.read(),
                                onclick: manage,
                                "Manage subscription\u{2026}"
                            }
                        }
                        if !current.is_pro() {
                            // The two that move you forward sit together on the
                            // right; the rest are housekeeping, on the left.
                            span { class: "licence-actions-gap" }
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
            if !account.products.is_empty() {
                p { "Apps included: {account.apps()}" }
            }
            p { "AI credits left this month: {credits.left} of {credits.granted}" }
            if let Some(r) = renewal {
                p { "{r}" }
            }
        }
    }
}
