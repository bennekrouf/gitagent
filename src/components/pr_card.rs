//! The open pull request for the selected repository, shown before you run
//! anything.
//!
//! Without this, four repositories all reading "ready to merge" are
//! indistinguishable, and deciding which one to act on means starting a full
//! review run in each just to find out what it contains. Everything here comes
//! from the probe's existing `gh pr view` call, so it costs nothing extra.

use dioxus::prelude::*;

use crate::services::checkhint::{self, Hint};
use crate::services::forge::Forge;
use crate::services::probe::{Checks, PrBrief};
use crate::services::review;

#[derive(Props, Clone, PartialEq)]
pub struct PrCardProps {
    pub pr: PrBrief,
    /// Where the pull request's repository is checked out: what working out
    /// why its checks failed, and updating it from its base, run in.
    pub repo: String,
    /// Where it lives: what closing it runs, and whether it can be closed
    /// from here at all.
    pub forge: Forge,
    /// False while a run is on this pull request: closing it under the run
    /// would leave the run merging something that is gone.
    pub closable: bool,
    /// Called once it is closed, to read the repository's pull requests again.
    pub on_closed: EventHandler<String>,
    /// The repository's base branch, the one its pull requests should go
    /// into. A pull request into any other branch is flagged on the card.
    /// Empty when it is not known yet, and then nothing is flagged.
    #[props(default)]
    pub base: String,
}

/// Where a pull request goes, as the card shows it: `from → into`, and a
/// warning when `into` is not the repository's base. `None` when the
/// forge did not name both branches.
pub fn route(pr: &PrBrief, base: &str) -> Option<(String, Option<String>)> {
    if pr.head.is_empty() || pr.base.is_empty() {
        return None;
    }
    let text = format!("{} \u{2192} {}", pr.head, pr.base);
    let off = (!base.is_empty() && pr.base != base).then(|| {
        format!(
            "Goes into {}, not {base}, this repository's base branch. Merging it \
             does not bring the change to {base}.",
            pr.base
        )
    });
    Some((text, off))
}

/// The button's word: Azure calls closing a pull request abandoning it.
fn close_word(forge: &Forge) -> Option<&'static str> {
    match forge {
        Forge::GitHub => Some("Close"),
        Forge::AzureDevOps => Some("Abandon"),
        _ => None,
    }
}

/// What the card asks before closing, so a stray click cannot.
fn close_question(forge: &Forge, number: &str) -> String {
    match forge {
        Forge::AzureDevOps => format!(
            "Abandon #{number} without merging? Its branch is kept, and it can be \
             reactivated in Azure DevOps."
        ),
        _ => format!(
            "Close #{number} without merging? Its branch is kept, and it can be \
             reopened on GitHub."
        ),
    }
}

fn checks_label(checks: Checks) -> (&'static str, &'static str) {
    match checks {
        Checks::Passing => ("checks passing", "done"),
        Checks::Failing => ("checks failing", "failed"),
        Checks::Pending => ("checks running", "running"),
        Checks::Unknown => ("checks not read", "skipped"),
    }
}

#[component]
pub fn PrCard(props: PrCardProps) -> Element {
    let pr = props.pr.clone();
    let (label, css) = checks_label(pr.checks);

    // Why the checks failed, worked out once per failure (see `checkhint`).
    let mut hint = use_signal(|| None::<Hint>);
    let mut update = use_signal(|| None::<Result<String, String>>);
    let mut updating = use_signal(|| false);
    // Closing: asked, then running, then gone from the list.
    let mut asking = use_signal(|| false);
    let mut closing = use_signal(|| false);
    let close = close_word(&props.forge).filter(|_| props.closable);
    let repo = props.repo.clone();
    let watched = pr.clone();
    use_effect(use_reactive!(|repo, watched| {
        hint.set(None);
        update.set(None);
        if watched.checks != Checks::Failing || repo.is_empty() {
            return;
        }
        let (repo, watched) = (repo.clone(), watched.clone());
        spawn(async move {
            hint.set(Some(checkhint::why_failed(&repo, &watched).await));
        });
    }));

    rsx! {
        div { class: "pr-card",
            div { class: "pr-card-head",
                span { class: "pr-number", "#{pr.number}" }
                span { class: "pr-title", "{pr.title}" }
            }
            if let Some((text, off)) = route(&pr, &props.base) {
                div {
                    class: if off.is_some() { "pr-route pr-route-off" } else { "pr-route" },
                    title: off.clone().unwrap_or_else(|| format!("From {} into {}", pr.head, pr.base)),
                    span { class: "pr-route-text", "\u{2442} {text}" }
                    if off.is_some() {
                        span { class: "pr-route-warn", "not the base" }
                    }
                }
            }
            div { class: "pr-card-meta",
                span { class: "pr-size", "{pr.size()}" }
                if pr.commits > 0 {
                    span { class: "pr-dot", "·" }
                    span { "{pr.commits} commit" }
                    if pr.commits != 1 { span { "s" } }
                }
            }
            div { class: "pr-card-foot",
                span { class: "dot dot-{css}" }
                // Failing is the state you want to go and look at, so it is
                // the one that goes somewhere: the pull request's Checks tab.
                if pr.checks == Checks::Failing && !pr.url.is_empty() {
                    a {
                        class: "status-{css} pr-checks-link",
                        href: "{checks_page(&pr.url)}",
                        target: "_blank",
                        title: "Open this pull request's checks",
                        onclick: |evt: Event<MouseData>| evt.stop_propagation(),
                        "{label}"
                    }
                } else {
                    span { class: "status-{css}", "{label}" }
                }
                if !pr.url.is_empty() {
                    a {
                        class: "pr-link",
                        href: "{pr.url}",
                        target: "_blank",
                        "open ↗"
                    }
                }
                if let Some(word) = close {
                    button {
                        class: if pr.url.is_empty() { "pr-close pr-close-alone" } else { "pr-close" },
                        disabled: *closing.read(),
                        title: "{word} this pull request without merging it",
                        onclick: move |evt: Event<MouseData>| {
                            evt.stop_propagation();
                            asking.set(true);
                        },
                        "{word}"
                    }
                }
            }
            if let (Some(word), true) = (close, *asking.read()) {
                div { class: "pr-hint",
                    onclick: |evt: Event<MouseData>| evt.stop_propagation(),
                    span { class: "pr-hint-text", "{close_question(&props.forge, &pr.number)}" }
                    button {
                        class: "btn btn-danger pr-hint-btn",
                        disabled: *closing.read(),
                        onclick: {
                            let repo = props.repo.clone();
                            let forge = props.forge.clone();
                            let number = pr.number.clone();
                            let empty = pr.files == 0 && forge == Forge::GitHub;
                            let on_closed = props.on_closed;
                            move |_| {
                                let (repo, forge, number) = (repo.clone(), forge.clone(), number.clone());
                                closing.set(true);
                                spawn(async move {
                                    let done = review::close_pr(&repo, &forge, &number, empty).await;
                                    closing.set(false);
                                    asking.set(false);
                                    let ok = done.is_ok();
                                    update.set(Some(done));
                                    if ok {
                                        on_closed.call(number);
                                    }
                                });
                            }
                        },
                        if *closing.read() { "{word}\u{2026}" } else { "{word} #{pr.number}" }
                    }
                    button {
                        class: "btn pr-hint-btn",
                        disabled: *closing.read(),
                        onclick: move |_| asking.set(false),
                        "Keep it"
                    }
                }
            }
            // What failed, in a sentence, and the one thing to do about it.
            match hint.read().clone() {
                Some(Hint::Behind { base, count }) => rsx! {
                    div { class: "pr-hint",
                        span { class: "pr-hint-text",
                            "Checks failed. {base} has {count} newer commit"
                            if count != 1 { "s" }
                            " that may already fix this."
                        }
                        button {
                            class: "btn pr-hint-btn",
                            disabled: *updating.read(),
                            title: "Merge the latest {base} into this pull request's branch and push it, so its checks run again. Done aside: your checked-out branch and files are not touched.",
                            onclick: {
                                let repo = props.repo.clone();
                                let pr = pr.clone();
                                move |evt: Event<MouseData>| {
                                    evt.stop_propagation();
                                    let (repo, pr) = (repo.clone(), pr.clone());
                                    updating.set(true);
                                    spawn(async move {
                                        let done = checkhint::update_from_base(&repo, &pr).await;
                                        checkhint::forget(&repo, &pr);
                                        update.set(Some(done));
                                        updating.set(false);
                                    });
                                }
                            },
                            if *updating.read() { "Updating\u{2026}" } else { "Update from {base}" }
                        }
                    }
                },
                Some(Hint::Error(line)) => rsx! {
                    div { class: "pr-hint",
                        span { class: "pr-hint-text", "Checks failed: " code { "{line}" } }
                        if let Some(first) = pr.failed.iter().find(|f| !f.url.is_empty()) {
                            a {
                                class: "btn pr-hint-btn",
                                href: "{first.url}",
                                target: "_blank",
                                onclick: |evt: Event<MouseData>| evt.stop_propagation(),
                                "See the error \u{2197}"
                            }
                        }
                    }
                },
                _ => rsx! {},
            }
            match update.read().clone() {
                Some(Ok(msg)) => rsx! { div { class: "pr-hint-done", "{msg}" } },
                Some(Err(msg)) => rsx! { div { class: "pr-hint-err", "{msg}" } },
                None => rsx! {},
            }
            // Which checks, each straight to its own log. Without this the
            // card can only say that something is red, and finding what means
            // leaving for the browser and hunting.
            if !pr.failed.is_empty() {
                div { class: "pr-failed",
                    for check in pr.failed.iter().take(MOST_FAILED_SHOWN).cloned() {
                        if check.url.is_empty() {
                            span { key: "{check.name}", class: "pr-failed-check", "\u{2715} {check.name}" }
                        } else {
                            a {
                                key: "{check.name}",
                                class: "pr-failed-check",
                                href: "{check.url}",
                                target: "_blank",
                                title: "Open the log of {check.name}",
                                onclick: |evt: Event<MouseData>| evt.stop_propagation(),
                                "\u{2715} {check.name} \u{2197}"
                            }
                        }
                    }
                    if pr.failed.len() > MOST_FAILED_SHOWN {
                        a {
                            class: "pr-failed-more",
                            href: "{checks_page(&pr.url)}",
                            target: "_blank",
                            onclick: |evt: Event<MouseData>| evt.stop_propagation(),
                            "+{pr.failed.len() - MOST_FAILED_SHOWN} more"
                        }
                    }
                }
            }
        }
    }
}

/// Enough to name what broke; a long matrix of red goes to the checks page.
const MOST_FAILED_SHOWN: usize = 4;

/// The Checks tab of a GitHub pull request, from its page.
fn checks_page(pr_url: &str) -> String {
    format!("{}/checks", pr_url.trim_end_matches('/'))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn into(base: &str) -> PrBrief {
        PrBrief {
            number: "292".into(),
            title: "t".into(),
            url: String::new(),
            checks: Checks::Unknown,
            files: 0,
            additions: 0,
            deletions: 0,
            commits: 0,
            base: base.into(),
            head: "feat/version".into(),
            failed: vec![],
        }
    }

    #[test]
    fn a_card_says_where_the_pull_request_goes() {
        let (text, off) = route(&into("develop"), "develop").unwrap();
        assert_eq!(text, "feat/version \u{2192} develop");
        assert!(off.is_none());
    }

    #[test]
    fn a_pull_request_into_another_branch_than_the_base_is_flagged() {
        let (_, off) = route(&into("main"), "develop").unwrap();
        let off = off.expect("main is not the base");
        assert!(off.contains("Goes into main, not develop"), "{off}");
    }

    #[test]
    fn nothing_is_flagged_while_the_base_is_not_known() {
        assert!(route(&into("main"), "").unwrap().1.is_none());
        assert!(route(&into(""), "develop").is_none());
    }

    #[test]
    fn every_check_state_reads_as_words_rather_than_a_colour_alone() {
        assert_eq!(checks_label(Checks::Passing).0, "checks passing");
        assert_eq!(checks_label(Checks::Failing).0, "checks failing");
        assert_eq!(checks_label(Checks::Pending).0, "checks running");
        // Azure: never claim green for something nobody looked at.
        assert_eq!(checks_label(Checks::Unknown).0, "checks not read");
    }

    #[test]
    fn closing_is_offered_only_where_gitagent_can_and_asks_first() {
        assert_eq!(close_word(&Forge::GitHub), Some("Close"));
        assert_eq!(close_word(&Forge::AzureDevOps), Some("Abandon"));
        assert_eq!(close_word(&Forge::None), None);
        assert_eq!(close_word(&Forge::Unsupported("gitlab.com".into())), None);
        assert!(close_question(&Forge::GitHub, "243").starts_with("Close #243 without merging?"));
        assert!(close_question(&Forge::AzureDevOps, "243").contains("Azure DevOps"));
    }

    #[test]
    fn the_checks_page_is_the_pull_request_page_plus_checks() {
        assert_eq!(
            checks_page("https://github.com/o/r/pull/117"),
            "https://github.com/o/r/pull/117/checks"
        );
        assert_eq!(
            checks_page("https://github.com/o/r/pull/117/"),
            "https://github.com/o/r/pull/117/checks"
        );
    }
}
