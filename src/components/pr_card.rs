//! The open pull request for the selected repository, shown before you run
//! anything.
//!
//! Without this, four repositories all reading "ready to merge" are
//! indistinguishable, and deciding which one to act on means starting a full
//! review run in each just to find out what it contains. Everything here comes
//! from the probe's existing `gh pr view` call, so it costs nothing extra.

use dioxus::prelude::*;

use crate::services::checkhint::{self, Hint};
use crate::services::probe::{Checks, PrBrief};

#[derive(Props, Clone, PartialEq)]
pub struct PrCardProps {
    pub pr: PrBrief,
    /// Where the pull request's repository is checked out: what working out
    /// why its checks failed, and updating it from its base, run in.
    pub repo: String,
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

    #[test]
    fn every_check_state_reads_as_words_rather_than_a_colour_alone() {
        assert_eq!(checks_label(Checks::Passing).0, "checks passing");
        assert_eq!(checks_label(Checks::Failing).0, "checks failing");
        assert_eq!(checks_label(Checks::Pending).0, "checks running");
        // Azure: never claim green for something nobody looked at.
        assert_eq!(checks_label(Checks::Unknown).0, "checks not read");
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
