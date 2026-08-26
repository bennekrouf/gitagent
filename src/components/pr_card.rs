//! The open pull request for the selected repository, shown before you run
//! anything.
//!
//! Without this, four repositories all reading "ready to merge" are
//! indistinguishable, and deciding which one to act on means starting a full
//! review run in each just to find out what it contains. Everything here comes
//! from the probe's existing `gh pr view` call, so it costs nothing extra.

use dioxus::prelude::*;

use crate::services::probe::{Checks, PrBrief};

#[derive(Props, Clone, PartialEq)]
pub struct PrCardProps {
    pub pr: PrBrief,
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
                span { class: "status-{css}", "{label}" }
                if !pr.url.is_empty() {
                    a {
                        class: "pr-link",
                        href: "{pr.url}",
                        target: "_blank",
                        "open ↗"
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
    fn every_check_state_reads_as_words_rather_than_a_colour_alone() {
        assert_eq!(checks_label(Checks::Passing).0, "checks passing");
        assert_eq!(checks_label(Checks::Failing).0, "checks failing");
        assert_eq!(checks_label(Checks::Pending).0, "checks running");
        // Azure: never claim green for something nobody looked at.
        assert_eq!(checks_label(Checks::Unknown).0, "checks not read");
    }
}
