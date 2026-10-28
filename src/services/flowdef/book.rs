//! The book of flows: the built-in ones and the repository's own, read, saved
//! and looked up.

use super::*;

impl FlowBook {
    /// The two flows the app ships with. Also what "Restore defaults" restores,
    /// so this stays the definition of standard rather than a one-time seed.
    pub fn defaults() -> Self {
        Self {
            adopted: vec![
                ADOPTED_TEST_STEP.to_string(),
                ADOPTED_NOTES_STEPS.to_string(),
                ADOPTED_IDS_FROM_LABELS.to_string(),
            ],
            flows: vec![
                FlowDef {
                    id: "commit_and_pr".into(),
                    label: "Commit → PR".into(),
                    handles: vec!["uncommitted".into(), "unpushed_branch".into()],
                    nodes: vec![
                        NodeDef::from_catalogue("preflight", "preflight"),
                        dep(
                            NodeDef::from_catalogue("scan", "scan_changes"),
                            &["preflight"],
                        ),
                        dep(
                            NodeDef::from_catalogue("draft_commit", "draft_commit"),
                            &["scan"],
                        ),
                        // Off `scan` rather than off `draft_commit`, so the
                        // suite runs while the model is still writing the
                        // message instead of after it — and so a red test
                        // stops the run before any history is touched.
                        dep(NodeDef::from_catalogue("test", "run_tests"), &["scan"]),
                        dep(
                            NodeDef::from_catalogue("commit", "commit"),
                            &["draft_commit", "test"],
                        ),
                        // After the commit rather than beside it, so a run the
                        // commit approval stops never spends a model call on
                        // a description nobody will open.
                        dep(NodeDef::from_catalogue("draft_pr", "draft_pr"), &["commit"]),
                        dep(NodeDef::from_catalogue("push", "push"), &["commit"]),
                        dep(
                            NodeDef::from_catalogue("open_pr", "open_pr"),
                            &["push", "draft_pr"],
                        ),
                    ],
                },
                FlowDef {
                    id: "review_and_merge".into(),
                    label: "Review → Merge".into(),
                    handles: vec!["open_pull_request".into()],
                    nodes: vec![
                        // Reviewing needs the same credential check committing
                        // does: without it, `gh pr view` fails with a raw CLI
                        // error instead of a preflight with a Fix button.
                        NodeDef::from_catalogue("preflight", "preflight"),
                        dep(
                            NodeDef::from_catalogue("find_pr", "find_pr"),
                            &["preflight"],
                        ),
                        dep(
                            NodeDef::from_catalogue("pr_status", "pr_status"),
                            &["find_pr"],
                        ),
                        dep(NodeDef::from_catalogue("pr_diff", "pr_diff"), &["find_pr"]),
                        dep(NodeDef::from_catalogue("analyse", "analyse"), &["pr_diff"]),
                        // Beside the analysis, not after it: both read the
                        // same diff, and the merge waits for the two.
                        dep(
                            NodeDef::from_catalogue("second_opinion", "second_opinion"),
                            &["pr_diff"],
                        ),
                        dep(NodeDef::from_catalogue("lenses", "lenses"), &["pr_diff"]),
                        dep(
                            NodeDef::from_catalogue("merge", "merge"),
                            &["pr_status", "analyse", "second_opinion", "lenses"],
                        ),
                        dep(NodeDef::from_catalogue("sync", "sync"), &["merge"]),
                    ],
                },
            ],
        }
    }

    /// Reads `flows.toml`, falling back to the defaults when it is absent or
    /// unreadable. A corrupt file must not leave the app with no flows at all.
    pub fn load() -> Self {
        let path = crate::services::store::data_dir().join(FLOWS_FILE);
        let Ok(text) = std::fs::read_to_string(path) else {
            return Self::defaults();
        };
        match toml::from_str::<FlowBook>(&text) {
            Ok(mut book) if !book.flows.is_empty() => {
                book.adopt_missing_declarations();
                // Before anything else reads an id. The per-repository lists
                // name flows by id too, so they follow the renames in the
                // same breath — a hidden flow must not come back as new.
                let renamed = book.adopt_ids_from_labels();
                if let Some(renames) = renamed.as_ref().filter(|r| !r.is_empty()) {
                    let mut shown = crate::services::store::load_repo_flows();
                    for (old, new) in renames {
                        shown.rename_flow(old, new);
                    }
                    crate::services::store::save_repo_flows(&shown);
                }
                // `|` rather than `||`: each adoption must get its turn.
                if book.adopt_missing_test_step()
                    | book.adopt_notes_steps()
                    | book.adopt_second_opinion()
                    | book.adopt_lenses()
                    | renamed.is_some()
                {
                    book.save();
                }
                book
            }
            _ => Self::defaults(),
        }
    }

    pub fn save(&self) {
        let dir = crate::services::store::data_dir();
        let _ = std::fs::create_dir_all(&dir);
        if let Ok(text) = toml::to_string_pretty(self) {
            let _ = crate::services::store::write_atomic(&dir.join(FLOWS_FILE), text.as_bytes());
        }
    }

    /// Fills in `handles` for shipped flows saved before the field existed.
    ///
    /// Without this, an existing `flows.toml` loads with every flow declaring
    /// nothing, so no flow answers any need and selection silently falls back
    /// to whichever flow happens to be first — landing on "Commit → PR" for a
    /// repository whose only outstanding work is a release.
    ///
    /// Only ever fills an *empty* list, so a deliberate choice to clear the
    /// declarations is left alone.
    pub fn adopt_missing_declarations(&mut self) {
        let shipped = Self::defaults();
        for flow in &mut self.flows {
            if flow.handles.is_empty() {
                if let Some(original) = shipped.get(&flow.id) {
                    flow.handles = original.handles.clone();
                }
            }
        }
    }

    /// Adds the test step to every commit-shaped flow saved before that step
    /// existed. Returns whether the book now needs writing back.
    ///
    /// Shape, not equality. The first version of this only upgraded a flow
    /// still byte-identical to the shipped one, which is to say nobody's: a
    /// real `flows.toml` has a base pinned on preflight and a couple of extra
    /// nodes on the end, and it was silently skipped. Anything that scans a
    /// working tree and then commits it is a commit flow, whatever else has
    /// been wired into it.
    ///
    /// It runs once ever, recorded in `adopted`. A step you delete has to stay
    /// deleted, or the next launch quietly overrules you.
    pub fn adopt_missing_test_step(&mut self) -> bool {
        if self.adopted.iter().any(|a| a == ADOPTED_TEST_STEP) {
            return false;
        }
        self.adopted.push(ADOPTED_TEST_STEP.to_string());
        for flow in &mut self.flows {
            flow.insert_test_step();
        }
        // Even when no flow changed, the marker did — and it is only worth
        // anything if it survives the launch that set it.
        true
    }

    /// Adds the release-notes steps to every flow that runs `release.sh`,
    /// once ever — same contract as `adopt_missing_test_step`.
    pub fn adopt_notes_steps(&mut self) -> bool {
        if self.adopted.iter().any(|a| a == ADOPTED_NOTES_STEPS) {
            return false;
        }
        self.adopted.push(ADOPTED_NOTES_STEPS.to_string());
        for flow in &mut self.flows {
            flow.insert_notes_steps();
        }
        true
    }

    /// Adds the second-opinion step to every flow that analyses a pull
    /// request, once ever — same contract as `adopt_missing_test_step`.
    pub fn adopt_second_opinion(&mut self) -> bool {
        if self.adopted.iter().any(|a| a == ADOPTED_SECOND_OPINION) {
            return false;
        }
        self.adopted.push(ADOPTED_SECOND_OPINION.to_string());
        for flow in &mut self.flows {
            flow.insert_second_opinion();
        }
        true
    }

    /// Adds the focused-reviews step the same way, once ever.
    pub fn adopt_lenses(&mut self) -> bool {
        if self.adopted.iter().any(|a| a == ADOPTED_LENSES) {
            return false;
        }
        self.adopted.push(ADOPTED_LENSES.to_string());
        for flow in &mut self.flows {
            flow.insert_lenses();
        }
        true
    }

    /// Gives every flow you made the id its name reads as, and returns each
    /// rename as `(old, new)` so whatever stores flow ids can follow. The
    /// shipped flows keep theirs: other code and other people's notes name
    /// them. A second flow of the same name gets `_2`, and an id already
    /// right — including a `_2` — is left alone.
    pub fn ids_from_labels(&mut self) -> Vec<(String, String)> {
        let shipped = Self::defaults();
        let mut renames = vec![];
        for i in 0..self.flows.len() {
            let id = self.flows[i].id.clone();
            if shipped.get(&id).is_some() {
                continue;
            }
            let want = id_from_label(&self.flows[i].label);
            let taken = |candidate: &str| {
                shipped.get(candidate).is_some()
                    || self
                        .flows
                        .iter()
                        .enumerate()
                        .any(|(j, f)| j != i && f.id == candidate)
            };
            let new = if !taken(&want) {
                want.clone()
            } else {
                (2..)
                    .map(|n| format!("{want}_{n}"))
                    .find(|c| c == &id || !taken(c))
                    .unwrap_or(want.clone())
            };
            if new != id {
                self.flows[i].id = new.clone();
                renames.push((id, new));
            }
        }
        renames
    }

    /// Once per book: see `ids_from_labels`. Returns the renames it made.
    pub fn adopt_ids_from_labels(&mut self) -> Option<Vec<(String, String)>> {
        if self.adopted.iter().any(|a| a == ADOPTED_IDS_FROM_LABELS) {
            return None;
        }
        self.adopted.push(ADOPTED_IDS_FROM_LABELS.to_string());
        Some(self.ids_from_labels())
    }

    pub fn get(&self, id: &str) -> Option<&FlowDef> {
        self.flows.iter().find(|f| f.id == id)
    }

    pub fn get_mut(&mut self, id: &str) -> Option<&mut FlowDef> {
        self.flows.iter_mut().find(|f| f.id == id)
    }

    pub fn free_flow_id(&self, base: &str) -> String {
        if !self.flows.iter().any(|f| f.id == base) {
            return base.to_string();
        }
        (2..)
            .map(|i| format!("{base}_{i}"))
            .find(|c| !self.flows.iter().any(|f| &f.id == c))
            .unwrap_or_else(|| base.to_string())
    }

    /// Flows that are safe to run — anything invalid is offered in Setup for
    /// repair, never handed to the executor.
    ///
    /// Repository-blind: every caller acting *for a repository* wants
    /// `runnable_for` instead, because a flow hidden on that repository is
    /// not something to run there however valid it is.
    pub fn runnable(&self) -> Vec<&FlowDef> {
        self.runnable_for(&[])
    }

    /// `runnable`, minus the flows hidden on this particular repository.
    ///
    /// Hiding is per-repository and lives in `repo_flows.json`, so the book
    /// alone cannot know about it — the ids have to be passed in. Anything
    /// choosing a flow on someone's behalf has to go through here: a hidden
    /// flow is a flow they said does not apply to this repository, and
    /// picking it for them anyway is the app overruling that.
    pub fn runnable_for(&self, hidden: &[String]) -> Vec<&FlowDef> {
        self.flows
            .iter()
            .filter(|f| validate(f).is_empty())
            .filter(|f| !hidden.iter().any(|id| id == &f.id))
            .collect()
    }

    /// Every flow, each with whatever is wrong with it.
    ///
    /// What the tab strip lists, as opposed to `runnable`, which is what may
    /// actually be started. A flow that stopped validating used to vanish from
    /// the strip altogether, which reads exactly like having deleted it — the
    /// flow is still in `flows.toml` and one edit from working, so it is shown
    /// and marked instead.
    pub fn listed(&self) -> Vec<(&FlowDef, Vec<Problem>)> {
        self.flows.iter().map(|f| (f, validate(f))).collect()
    }

    /// Copies a flow's nodes into a new flow, `_copy`/`_copy_2`/... suffixed.
    /// Returns the new id, or `None` if `id` does not name an existing flow.
    pub fn duplicate(&mut self, id: &str) -> Option<String> {
        let mut copy = self.get(id)?.clone();
        let new_id = self.free_flow_id(&format!("{id}_copy"));
        copy.id = new_id.clone();
        self.flows.push(copy);
        Some(new_id)
    }
}
