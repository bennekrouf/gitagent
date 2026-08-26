//! The vocabulary: every step type the app knows how to run.
//!
//! A flow is a wiring of these; it cannot invent new ones, because a step is
//! ultimately a Rust function. That split is deliberate — the editor gets to
//! rearrange the graph freely without ever being able to describe work that
//! does not exist.
//!
//! Note what the catalogue owns and the flow file does not: `reads` and
//! `writes`. Those are facts about what the implementation does, not choices.
//! Letting a flow file declare them would let it describe a contract the code
//! does not honour, and the whole point of the contract is that it is true.

use super::graph::{NodeKind, Step};

/// One setting a step takes per node, rendered as an input in Setup.
#[derive(Clone, PartialEq, Debug)]
pub struct ConfigField {
    pub key: &'static str,
    pub label: &'static str,
    pub placeholder: &'static str,
    pub help: &'static str,
    pub multiline: bool,
    /// A step with an empty required field cannot run, and validation says so.
    pub required: bool,
}

#[derive(Clone, PartialEq, Debug)]
pub struct StepInfo {
    pub step: Step,
    /// Stable identifier used in the flow file.
    pub key: &'static str,
    pub title: &'static str,
    pub subtitle: &'static str,
    /// What it is for, shown when picking a step to add.
    pub about: &'static str,
    pub kind: NodeKind,
    pub reads: &'static [&'static str],
    pub writes: &'static [&'static str],
    /// Whether this step touches history, a remote, or anything else that
    /// should stop for a human by default.
    pub gate_by_default: bool,
    /// Settings this step takes per node. Most steps take none.
    pub config: &'static [ConfigField],
    /// Whether Setup should offer a "Test connection" button for this step —
    /// for anything whose settings you cannot check by reading them.
    pub testable: bool,
}

/// Contract keys may name the node they belong to, so two copies of the same
/// configurable step in one flow do not collide on their outputs.
pub fn expand(keys: &[&'static str], node_id: &str) -> Vec<String> {
    keys.iter().map(|k| k.replace("{id}", node_id)).collect()
}

pub const CATALOGUE: &[StepInfo] = &[
    StepInfo {
        step: Step::Preflight,
        key: "preflight",
        title: "Preflight",
        subtitle: "Remote, forge, credentials, model",
        about: "Checks everything a run depends on before anything is touched: \
                the remote and which forge it is, the base branch, the platform \
                credentials, and that the model answers.",
        kind: NodeKind::Deterministic,
        reads: &[],
        writes: &["remote_url", "forge", "base"],
        gate_by_default: false,
        config: &[],
        testable: false,
    },
    StepInfo {
        step: Step::ScanChanges,
        key: "scan_changes",
        title: "Scan changes",
        subtitle: "Read the working tree",
        about: "Reads the uncommitted changes and produces the diff every later \
                step works from. New files get a diff too, without touching the index.",
        kind: NodeKind::Deterministic,
        reads: &["base"],
        writes: &[
            "branch",
            "stat",
            "diff",
            "commit_paths",
            "file_notes",
            "untracked",
        ],
        gate_by_default: false,
        config: &[],
        testable: false,
    },
    StepInfo {
        step: Step::DraftCommit,
        key: "draft_commit",
        title: "Draft commit message",
        subtitle: "Model call — subject, body, branch name",
        about: "Turns the diff into a Conventional Commits subject, a short body, \
                and a branch name.",
        kind: NodeKind::Model,
        reads: &["stat", "diff"],
        writes: &["branch_name", "commit_subject", "commit_body"],
        gate_by_default: false,
        config: &[],
        testable: false,
    },
    StepInfo {
        step: Step::Commit,
        key: "commit",
        title: "Commit",
        subtitle: "Branch if needed, stage, commit",
        about: "Opens a topic branch when sitting on a protected one, stages \
                exactly the files left checked at the approval, and commits.",
        kind: NodeKind::Deterministic,
        reads: &[
            "branch_name",
            "commit_subject",
            "commit_body",
            "commit_paths",
        ],
        writes: &["work_branch", "commit_sha"],
        gate_by_default: true,
        config: &[],
        testable: false,
    },
    StepInfo {
        step: Step::DraftPr,
        key: "draft_pr",
        title: "Draft PR description",
        subtitle: "Model call — title and body",
        about: "Writes the pull request title and body from the diff. Depends on \
                the diff, not on the push, so it can run alongside git.",
        kind: NodeKind::Model,
        reads: &["stat", "diff", "commit_subject"],
        writes: &["pr_title", "pr_body"],
        gate_by_default: false,
        config: &[],
        testable: false,
    },
    StepInfo {
        step: Step::Push,
        key: "push",
        title: "Push branch",
        subtitle: "git push -u origin <branch>",
        about: "Publishes the working branch to origin.",
        kind: NodeKind::Deterministic,
        reads: &["work_branch"],
        writes: &["push_output"],
        gate_by_default: true,
        config: &[],
        testable: false,
    },
    StepInfo {
        step: Step::OpenPr,
        key: "open_pr",
        title: "Open pull request",
        subtitle: "gh pr create / az repos pr create",
        about: "Opens the pull request on whichever forge the remote points at.",
        kind: NodeKind::Deterministic,
        reads: &["work_branch", "base", "pr_title", "pr_body"],
        writes: &["pr_url"],
        gate_by_default: true,
        config: &[],
        testable: false,
    },
    StepInfo {
        step: Step::FindPr,
        key: "find_pr",
        title: "Find the pull request",
        subtitle: "The open PR for this branch",
        about: "Looks up the open pull request whose source branch is the one \
                checked out.",
        kind: NodeKind::Deterministic,
        reads: &["forge"],
        writes: &["pr_number", "pr_title", "pr_url", "pr_base", "pr_head"],
        gate_by_default: false,
        config: &[],
        testable: false,
    },
    StepInfo {
        step: Step::PrStatus,
        key: "pr_status",
        title: "CI status",
        subtitle: "Checks and mergeability",
        about: "Reads the check rollup and merge state from the forge. GitHub only \
                so far; on Azure it reports that it did not look rather than \
                claiming green.",
        kind: NodeKind::Deterministic,
        reads: &["pr_number"],
        writes: &["checks_summary", "checks_state", "merge_state"],
        gate_by_default: false,
        config: &[],
        testable: false,
    },
    StepInfo {
        step: Step::PrDiff,
        key: "pr_diff",
        title: "Fetch the diff",
        subtitle: "git diff base...head",
        about: "Gets the pull request's diff from git rather than the forge, so it \
                works the same on every platform.",
        kind: NodeKind::Deterministic,
        reads: &["pr_base", "pr_head"],
        writes: &["pr_diff", "pr_stat"],
        gate_by_default: false,
        config: &[],
        testable: false,
    },
    StepInfo {
        step: Step::Analyse,
        key: "analyse",
        title: "Analyse for regressions",
        subtitle: "Model call — what could this break?",
        about: "Reviews the diff for concrete regressions. Every finding must quote \
                the diff verbatim; quotes that are not in the diff are dropped.",
        kind: NodeKind::Model,
        reads: &["pr_diff", "pr_stat", "pr_title"],
        writes: &["verdict", "analysis", "finding_count"],
        gate_by_default: false,
        config: &[],
        testable: false,
    },
    StepInfo {
        step: Step::Merge,
        key: "merge",
        title: "Merge",
        subtitle: "Squash and delete the branch",
        about: "Merges the pull request. The approval shows the CI state and the \
                model's verdict side by side; nothing blocks the merge on its own.",
        kind: NodeKind::Deterministic,
        reads: &["pr_number", "checks_summary", "verdict", "analysis"],
        writes: &["merge_output"],
        gate_by_default: true,
        config: &[],
        testable: false,
    },
    StepInfo {
        step: Step::Sync,
        key: "sync",
        title: "Back to base",
        subtitle: "Checkout the base branch and pull",
        about: "Returns to the base branch and fast-forwards it.",
        kind: NodeKind::Deterministic,
        reads: &["pr_base"],
        writes: &["sync_output"],
        gate_by_default: false,
        config: &[],
        testable: false,
    },
    StepInfo {
        step: Step::RunScript,
        key: "run_script",
        title: "Run a script",
        subtitle: "A command in the repository",
        about: "Runs any command in the repository root — a release script, a \
                test suite, a deploy. Its output becomes an artifact named after \
                this step, so a later step can read it.",
        kind: NodeKind::Deterministic,
        reads: &[],
        writes: &["{id}_output", "{id}_exit"],
        gate_by_default: true,
        testable: false,
        config: &[
            ConfigField {
                key: "command",
                label: "Command",
                placeholder: "./scripts/release.sh --patch",
                help: "Run through `sh -c` in the repository root, so pipes and \
                       arguments work as they do in a terminal.",
                multiline: false,
                required: true,
            },
            ConfigField {
                key: "stdin",
                label: "Answer prompts with",
                placeholder: "y",
                help: "Sent to the command's stdin. A script that asks for \
                       confirmation needs this, because it has no terminal to ask \
                       through — and GitAgent already asked you at the approval.",
                multiline: true,
                required: false,
            },
        ],
    },
    StepInfo {
        step: Step::RunRemote,
        key: "run_remote",
        title: "Run on a server",
        subtitle: "A command over ssh",
        about: "Runs a command on another machine over ssh — a deploy, a smoke \
                test against staging, a service restart. GitAgent stores no keys: \
                it shells out to your own ssh, so ~/.ssh/config aliases, agent \
                forwarding and known_hosts all work as they do in a terminal.",
        kind: NodeKind::Deterministic,
        reads: &[],
        writes: &["{id}_output", "{id}_exit"],
        gate_by_default: true,
        testable: true,
        config: &[
            ConfigField {
                key: "host",
                label: "Host",
                placeholder: "deploy@staging.example.com",
                help: "`user@host`, or any alias from your ~/.ssh/config.",
                multiline: false,
                required: true,
            },
            ConfigField {
                key: "command",
                label: "Command",
                placeholder: "cd /srv/app && ./deploy.sh",
                help: "Run by the login shell on the remote host.",
                multiline: true,
                required: true,
            },
            ConfigField {
                key: "identity",
                label: "Identity file",
                placeholder: "~/.ssh/id_ed25519",
                help: "Optional. Leave empty to let ssh and your agent choose, \
                       which is usually the right answer.",
                multiline: false,
                required: false,
            },
            ConfigField {
                key: "port",
                label: "Port",
                placeholder: "22",
                help: "Optional.",
                multiline: false,
                required: false,
            },
            ConfigField {
                key: "stdin",
                label: "Answer prompts with",
                placeholder: "y",
                help: "Sent to the remote command's stdin.",
                multiline: true,
                required: false,
            },
        ],
    },
];

/// Used by the exhaustiveness test to prove every variant is described.
#[cfg(test)]
pub fn info(step: Step) -> &'static StepInfo {
    CATALOGUE
        .iter()
        .find(|i| i.step == step)
        .expect("every Step has a catalogue entry — see the exhaustiveness test")
}

pub fn by_key(key: &str) -> Option<&'static StepInfo> {
    CATALOGUE.iter().find(|i| i.key == key)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The one test that keeps the catalogue honest: adding a `Step` variant
    /// without describing it fails here rather than panicking at runtime.
    #[test]
    fn every_step_variant_has_a_catalogue_entry() {
        let all = [
            Step::Preflight,
            Step::ScanChanges,
            Step::DraftCommit,
            Step::Commit,
            Step::DraftPr,
            Step::Push,
            Step::OpenPr,
            Step::FindPr,
            Step::PrStatus,
            Step::PrDiff,
            Step::Analyse,
            Step::Merge,
            Step::Sync,
            Step::RunScript,
            Step::RunRemote,
        ];
        assert_eq!(
            all.len(),
            CATALOGUE.len(),
            "a Step is missing from the catalogue"
        );
        for step in all {
            let _ = info(step);
        }
    }

    #[test]
    fn a_configurable_steps_outputs_are_named_after_the_node() {
        // Two release steps in one flow must not overwrite each other.
        let info = by_key("run_script").unwrap();
        assert_eq!(
            expand(info.writes, "release"),
            vec!["release_output".to_string(), "release_exit".to_string()]
        );
        assert_eq!(
            expand(info.writes, "smoke"),
            vec!["smoke_output".to_string(), "smoke_exit".to_string()]
        );
    }

    #[test]
    fn a_step_with_no_template_is_left_alone() {
        let info = by_key("commit").unwrap();
        assert_eq!(
            expand(info.writes, "anything"),
            vec!["work_branch".to_string(), "commit_sha".to_string()]
        );
    }

    #[test]
    fn running_an_arbitrary_command_is_gated_by_default() {
        assert!(by_key("run_script").unwrap().gate_by_default);
        assert!(by_key("run_remote").unwrap().gate_by_default);
    }

    #[test]
    fn only_the_settings_you_cannot_verify_by_reading_offer_a_test() {
        assert!(by_key("run_remote").unwrap().testable);
        assert!(!by_key("run_script").unwrap().testable);
        assert!(!by_key("commit").unwrap().testable);
    }

    #[test]
    fn the_remote_step_asks_for_a_host_but_not_for_a_key() {
        let info = by_key("run_remote").unwrap();
        let required: Vec<&str> = info
            .config
            .iter()
            .filter(|f| f.required)
            .map(|f| f.key)
            .collect();
        assert_eq!(required, vec!["host", "command"]);
        // A key is optional on purpose: ssh and the agent usually know better.
        assert!(info
            .config
            .iter()
            .any(|f| f.key == "identity" && !f.required));
    }

    #[test]
    fn keys_are_unique_so_a_flow_file_is_unambiguous() {
        let mut keys: Vec<&str> = CATALOGUE.iter().map(|i| i.key).collect();
        let count = keys.len();
        keys.sort_unstable();
        keys.dedup();
        assert_eq!(keys.len(), count);
    }

    #[test]
    fn keys_round_trip() {
        for entry in CATALOGUE {
            assert_eq!(by_key(entry.key).unwrap().step, entry.step);
        }
    }

    #[test]
    fn an_unknown_key_is_none_rather_than_a_panic() {
        assert!(by_key("does_not_exist").is_none());
    }

    #[test]
    fn everything_that_writes_to_a_remote_is_gated_by_default() {
        for key in ["commit", "push", "open_pr", "merge"] {
            assert!(by_key(key).unwrap().gate_by_default, "{key}");
        }
    }

    #[test]
    fn read_only_steps_are_not_gated_by_default() {
        for key in [
            "preflight",
            "scan_changes",
            "pr_status",
            "pr_diff",
            "analyse",
        ] {
            assert!(!by_key(key).unwrap().gate_by_default, "{key}");
        }
    }
}
