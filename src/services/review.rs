//! The review-and-merge flow: take an open pull request, put its CI status and
//! a model's read of the diff side by side, and let the human merge it.
//!
//! ```text
//!                ┌──> pr_status ──┐
//!   find_pr ──>  ┤                ├──> merge ──> sync
//!                └──> pr_diff ──> analyse ──┘
//! ```
//!
//! `pr_status` asks the forge; `pr_diff` asks git. Neither reads the other, so
//! they are siblings rather than a sequence. `merge` joins them because the
//! decision needs both — a green build the model dislikes, and a red build the
//! model is happy with, are different situations and the approval shows which
//! one you are in.
//!
//! Nothing here blocks a merge on its own. Their `fmt` check is deliberately
//! non-blocking, so a rule like "refuse when any check is red" would refuse
//! every pull request they open. The gate is the human, given the full picture.

use serde_json::json;

use super::flow::{self, StepFailure, StepOutcome};
use super::forge::{self, Forge, Scope};
use super::git;
use super::graph::{Remedy, RunState, Step};
use super::llm::{complete_json, Asker, LlmConfig};
mod pr;
pub use pr::*;
mod sync;
pub use sync::*;

/// The merge approval is the whole point of this flow: both signals, stated
/// plainly, with the disagreement visible when there is one.
pub fn proposal(step: Step, state: &RunState) -> String {
    match step {
        Step::Merge => format!(
            "{}{}{}",
            merge_proposal(state),
            second_opinion(state),
            focused(state)
        ),
        _ => String::new(),
    }
}

/// The second model's verdict under the first's, when there is one — the
/// disagreement between them is as worth seeing as the one between model and
/// CI.
fn second_opinion(state: &RunState) -> String {
    let verdict = state.artifact("second_verdict");
    if verdict.is_empty() {
        return String::new();
    }
    format!(
        "\n\n── Second opinion ({}) ──────────────\nverdict: {}\n\n{}",
        state.artifact("second_model"),
        verdict,
        state.artifact("second_analysis"),
    )
}

/// Each focused review that ran, under the rest.
fn focused(state: &RunState) -> String {
    LENSES
        .into_iter()
        .filter(|l| !state.artifact(&format!("{}_verdict", l.key())).is_empty())
        .map(|l| {
            format!(
                "\n\n── {} ──────────────────────\nverdict: {}\n\n{}",
                l.label(),
                state.artifact(&format!("{}_verdict", l.key())),
                state.artifact(&format!("{}_analysis", l.key())),
            )
        })
        .collect()
}

fn merge_proposal(state: &RunState) -> String {
    format!(
        "gh pr merge {} --squash --delete-branch\n\n\
         ── CI ────────────────────────────────\n{}\n\
         merge state: {}\n{}\n\
         ── Model ─────────────────────────────\nverdict: {}\n\n{}",
        state.artifact("pr_number"),
        state.artifact("checks_summary"),
        state.artifact("merge_state"),
        state.artifact("checks_detail"),
        state.artifact("verdict"),
        state.artifact("analysis"),
    )
}

pub async fn execute(
    step: Step,
    repo: &str,
    cfg: &LlmConfig,
    state: &RunState,
    asker: &mut Asker<'_>,
) -> Result<StepOutcome, StepFailure> {
    match step {
        Step::FindPr => find_pr(repo, state).await,
        Step::PrStatus => pr_status(repo, state).await,
        Step::PrDiff => pr_diff(repo, state).await,
        Step::Analyse => analyse(cfg, state, asker, REGRESSIONS, "").await,
        // The same review, asked of the second model. The step is only ever
        // reached with one chosen: with none, the run marks it skipped first.
        Step::SecondOpinion => {
            analyse(&cfg.second_config(), state, asker, REGRESSIONS, "second_").await
        }
        // Likewise only reached with at least one lens turned on.
        Step::Lenses => lenses(cfg, state, asker).await,
        Step::Merge => merge(repo, state).await,
        Step::Sync => sync(repo, state).await,
        _ => Err(StepFailure::from("step does not belong to this flow")),
    }
}

/// The job id out of a check's `detailsUrl`
/// (`…/actions/runs/<run>/job/<job>`).
fn job_id(details_url: &str) -> Option<&str> {
    details_url
        .rsplit_once("/job/")
        .map(|(_, id)| id)
        .filter(|id| !id.is_empty() && id.chars().all(|c| c.is_ascii_digit()))
}

/// Reviews the diff with `cfg`'s model. `prefix` keeps a second opinion's
/// results apart from the first's: `verdict` and `second_verdict`, and so on,
/// so neither overwrites the other and each is read for what it is.
/// What the regression review looks for — the first review, and the second
/// opinion, which asks a second model the same question.
const REGRESSIONS: &str = "You review a pull request diff for regressions.";

/// A focused review: one question asked of the whole diff, beside the
/// regression review rather than instead of it. Each is its own model call, so
/// each is turned on separately in Settings.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Lens {
    Alignment,
    Security,
    Architecture,
}

pub const LENSES: [Lens; 3] = [Lens::Alignment, Lens::Security, Lens::Architecture];

impl Lens {
    /// Names its artifacts — `security_verdict` — and its switch in Settings.
    pub fn key(self) -> &'static str {
        match self {
            Lens::Alignment => "alignment",
            Lens::Security => "security",
            Lens::Architecture => "architecture",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Lens::Alignment => "Alignment",
            Lens::Security => "Security",
            Lens::Architecture => "Architecture",
        }
    }

    /// One line for Settings: what turning it on buys.
    pub fn about(self) -> &'static str {
        match self {
            Lens::Alignment => "does the change do what its title says, and nothing unrelated",
            Lens::Security => "vulnerabilities the change introduces",
            Lens::Architecture => "layering, coupling and logic in the wrong place",
        }
    }

    fn focus(self) -> &'static str {
        match self {
            Lens::Alignment => {
                "You check whether a pull request diff does what its title says it does. \
                 A finding is a place where the diff goes beyond the title (unrelated \
                 changes), falls short of it (something the title promises that is missing), \
                 or contradicts it."
            }
            Lens::Security => {
                "You review a pull request diff for security vulnerabilities it introduces: \
                 injection, unchecked input reaching a shell, query or path, secrets or \
                 credentials in code, missing authorisation, unsafe deserialisation."
            }
            Lens::Architecture => {
                "You review a pull request diff for architectural problems it introduces: \
                 logic placed in the wrong layer or module, new coupling between parts that \
                 were separate, duplicated logic, and abstractions inconsistent with the \
                 surrounding code shown in the diff."
            }
        }
    }
}

/// Runs every lens turned on in Settings, one after another, so a local model
/// is never asked three things at once. Each writes its own results under its
/// own name, and `lenses_run` says which ran.
async fn lenses(
    cfg: &LlmConfig,
    state: &RunState,
    asker: &mut Asker<'_>,
) -> Result<StepOutcome, StepFailure> {
    let mut artifacts = vec![];
    let mut summary = vec![];
    let mut log = vec![];
    let mut ran = vec![];
    for lens in LENSES.into_iter().filter(|l| cfg.lenses.is_on(l.key())) {
        (asker.on_line)(&format!(
            "\u{2500}\u{2500} {} \u{2500}\u{2500}",
            lens.label()
        ));
        let prefix = format!("{}_", lens.key());
        let out = analyse(cfg, state, asker, lens.focus(), &prefix).await?;
        summary.push(format!("{} {}", lens.label().to_lowercase(), out.summary));
        log.push(format!(
            "\u{2500}\u{2500} {} \u{2500}\u{2500}\n{}",
            lens.label(),
            out.log
        ));
        artifacts.extend(out.artifacts);
        ran.push(lens.key());
    }
    artifacts.push(("lenses_run".into(), ran.join(",")));
    Ok(StepOutcome {
        summary: summary.join(" \u{00b7} "),
        log: log.join("\n\n"),
        artifacts,
        nothing_to_do: false,
        items: vec![],
    })
}

/// Whether `evidence` is really text from `diff`. Models quote the code, not
/// the diff around it: they leave off each line's `+`/`-`, join two lines
/// into one, or re-indent. An exact match threw out correct findings for
/// that, so both sides are compared without line markers and with every run
/// of whitespace as one space. A quote that is not in the diff — invented, or
/// garbled in the retelling — still does not match.
fn quoted_from(diff: &str, evidence: &str) -> bool {
    let evidence = evidence
        .trim()
        .trim_matches(|c| c == '`' || c == '"')
        .trim();
    if evidence.is_empty() {
        return false;
    }
    if diff.contains(evidence) {
        return true;
    }
    fn flatten(text: &str, is_diff: bool) -> String {
        text.lines()
            .filter(|l| !(is_diff && (l.starts_with("+++") || l.starts_with("---"))))
            .map(|l| {
                if is_diff {
                    l.strip_prefix(['+', '-', ' ']).unwrap_or(l)
                } else {
                    l.trim_start().strip_prefix(['+', '-']).unwrap_or(l)
                }
            })
            .flat_map(str::split_whitespace)
            .collect::<Vec<_>>()
            .join(" ")
    }
    let wanted = flatten(evidence, false);
    !wanted.is_empty() && flatten(diff, true).contains(&wanted)
}

async fn analyse(
    cfg: &LlmConfig,
    state: &RunState,
    asker: &mut Asker<'_>,
    focus: &str,
    prefix: &str,
) -> Result<StepOutcome, StepFailure> {
    let system = format!(
        "{focus}\n\
        Report only what the diff itself shows. Rules:\n\
        - `verdict`: `looks_safe` if you found nothing concrete, `worth_a_look` for \
          plausible problems, `risky` for a specific likely break.\n\
        - Each finding needs a `claim` (the defect in one sentence), a `trigger` \
          (concrete inputs or state that produce it), and `evidence` quoted \
          VERBATIM from the diff above. If you cannot quote the diff, do not \
          report the finding.\n\
        - Report nothing rather than something vague. \"Consider adding tests\" and \
          \"verify error handling\" are not findings."
    );

    let head = format!(
        "Title: {}\n\nDiffstat:\n{}\n\nDiff:\n",
        state.artifact("pr_title"),
        state.artifact("pr_stat"),
    );
    let fitted = git::fit(
        state.artifact("pr_diff"),
        cfg.input_budget().saturating_sub(head.len()),
    );
    let user = format!("{head}{}", fitted.text);

    // Every string and the list are bounded. A small model at temperature 0
    // can fall into repeating one token inside a string — a 2B model quoting
    // code wrote `\`` until the answer budget ran out — and the cut-off
    // reply is not JSON at all. ollama enforces these bounds while it
    // generates, so a loop is forced to close its string and move on; the
    // longest answer they allow still fits the budget.
    let schema = json!({
        "type": "object",
        "properties": {
            "verdict": { "type": "string", "enum": ["looks_safe", "worth_a_look", "risky"] },
            "summary": { "type": "string", "maxLength": 600 },
            "findings": {
                "type": "array",
                "maxItems": 6,
                "items": {
                    "type": "object",
                    "properties": {
                        "severity": { "type": "string", "enum": ["high", "medium", "low"] },
                        "file":     { "type": "string", "maxLength": 200 },
                        "claim":    { "type": "string", "maxLength": 300 },
                        "trigger":  { "type": "string", "maxLength": 300 },
                        "evidence": { "type": "string", "maxLength": 400 }
                    },
                    "required": ["severity", "file", "claim", "trigger", "evidence"]
                }
            }
        },
        "required": ["verdict", "summary", "findings"]
    });

    let value = complete_json(cfg, &system, &user, &schema, asker).await?;
    let verdict = value["verdict"]
        .as_str()
        .unwrap_or("worth_a_look")
        .to_string();
    let summary = value["summary"]
        .as_str()
        .unwrap_or_default()
        .trim()
        .to_string();
    let diff = state.artifact("pr_diff");

    let mut kept = vec![];
    let mut dropped = 0;
    for finding in value["findings"].as_array().cloned().unwrap_or_default() {
        let evidence = finding["evidence"].as_str().unwrap_or_default();
        // The cheapest hallucination filter there is: if the quote is not in
        // what we sent, the finding is about code that does not exist.
        if !quoted_from(diff, evidence) {
            dropped += 1;
            continue;
        }
        kept.push(format!(
            "[{}] {}\n     file:    {}\n     trigger: {}\n     evidence: {}",
            finding["severity"].as_str().unwrap_or("low"),
            finding["claim"].as_str().unwrap_or_default(),
            finding["file"].as_str().unwrap_or_default(),
            finding["trigger"].as_str().unwrap_or_default(),
            evidence.trim(),
        ));
    }

    let mut analysis = summary.clone();
    if !kept.is_empty() {
        analysis.push_str(&format!("\n\n{}", kept.join("\n\n")));
    }
    if dropped > 0 {
        analysis.push_str(&format!(
            "\n\n({dropped} finding(s) dropped — their quoted evidence was not in the diff.)"
        ));
    }

    let model = cfg.active_model().to_string();
    let mut artifacts = vec![
        (format!("{prefix}verdict"), verdict.clone()),
        (format!("{prefix}analysis"), analysis.clone()),
        (format!("{prefix}finding_count"), kept.len().to_string()),
    ];
    if !prefix.is_empty() {
        artifacts.push((format!("{prefix}model"), model.clone()));
    }
    Ok(StepOutcome {
        summary: if prefix.is_empty() {
            format!("{verdict} · {} finding(s)", kept.len())
        } else {
            format!("{model}: {verdict} · {} finding(s)", kept.len())
        },
        log: format!("{analysis}{}", fitted.note()),
        artifacts,
        nothing_to_do: false,
        items: vec![],
    })
}

/// After a push, GitHub works out mergeability a few seconds later and can
/// still report the old conflict meanwhile — merging again straight away would
/// fail on stale news. `$1` is the pull request number.
const WAIT_FOR_GITHUB: &str = r#"for i in 1 2 3 4 5 6 7 8 9 10; do
  sleep 3
  state=$(gh pr view "$1" --json mergeable -q .mergeable)
  [ "$state" = MERGEABLE ] && exit 0
  if [ "$state" = CONFLICTING ] && [ "$i" -gt 3 ]; then
    echo "Pushed, but GitHub still reports #$1 as conflicting."; exit 1
  fi
done
echo "Pushed. GitHub has not finished rechecking #$1; merging anyway.""#;

/// What to do about edits that clashed when they were put back. The step is
/// done either way; this is the person's to finish in their editor.
fn clashes_note(paths: &[String]) -> String {
    format!(
        "Your edits clashed with the update in {}:\n\n  {}\n\nBoth versions are \
         marked in each file. Keep what you want, `git add` the file, and once all \
         are resolved, `git stash drop` removes the copy kept in the stash.",
        plural_files(paths.len()),
        paths.join("\n  ")
    )
}

fn plural_files(n: usize) -> String {
    if n == 1 {
        "1 file".into()
    } else {
        format!("{n} files")
    }
}

#[cfg(test)]
mod tests;
