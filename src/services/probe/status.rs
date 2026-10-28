//! What a repository needs now, and which flows it can run.

use super::*;

/// What the repository wants from you, most urgent first.
///
/// The ordering is the whole point: it decides which repository the app opens
/// on, so it is stated once, here, and tested.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum Wants {
    /// A rebase, merge or cherry-pick stopped part-way. Nothing else in the
    /// repository can move until it is finished or abandoned, so it outranks
    /// everything — including a decision on a pull request.
    Resolve,
    /// A pull request whose checks are green — a decision is waiting.
    Merge,
    /// A pull request whose checks are red.
    Attention,
    /// Uncommitted work.
    Commit,
    /// Committed work on a topic branch with no pull request behind it.
    OpenPr,
    /// Merged work that has not been tagged.
    Release,
    /// A pull request whose checks are still running. Nothing to do yet.
    Wait,
    /// Clean tree, no pull request.
    Nothing,
}

impl Wants {
    /// Whether a person is actually expected to do something.
    pub fn needs_a_person(self) -> bool {
        matches!(
            self,
            Wants::Resolve
                | Wants::Merge
                | Wants::Attention
                | Wants::Commit
                | Wants::OpenPr
                | Wants::Release
        )
    }

    pub fn note(self) -> &'static str {
        match self {
            Wants::Resolve => "unfinished rebase",
            Wants::Merge => "ready to merge",
            Wants::Attention => "checks failing",
            Wants::Commit => "uncommitted",
            Wants::OpenPr => "needs a PR",
            Wants::Release => "release due",
            Wants::Wait => "checks running",
            Wants::Nothing => "clean",
        }
    }

    /// Whether this state is worth any words in a list of repositories.
    ///
    /// The two resting states get none. A column where every clean repository
    /// says CLEAN is a column you have to read to find the one that does not —
    /// which is exactly backwards. The dot still carries the state for anyone
    /// who wants it.
    pub fn is_worth_saying(self) -> bool {
        !matches!(self, Wants::Nothing | Wants::Wait)
    }

    /// A glyph for the states worth saying something about, so a row can be
    /// sorted by eye before it is read.
    pub fn icon(self) -> &'static str {
        match self {
            Wants::Resolve => "\u{26a0}",
            Wants::Merge => "\u{2713}",
            Wants::Attention => "\u{2715}",
            Wants::Commit => "\u{270e}",
            Wants::OpenPr => "\u{2197}",
            Wants::Release => "\u{2191}",
            Wants::Wait | Wants::Nothing => "",
        }
    }

    pub fn css(self) -> &'static str {
        match self {
            Wants::Resolve => "failed",
            Wants::Merge => "done",
            Wants::Attention => "failed",
            Wants::Commit => "awaiting",
            Wants::OpenPr => "awaiting",
            Wants::Release => "running",
            Wants::Wait => "running",
            Wants::Nothing => "skipped",
        }
    }

    /// Which flow to open on when this repository is chosen.
    /// The kind of work this state represents, which a flow declares it
    /// answers. Names a *need*, not a flow — so "Deploy VPS" can answer the
    /// release need without being called `release`.
    ///
    /// `None` for states nobody acts on.
    pub fn need(self) -> Option<Need> {
        match self {
            Wants::Commit => Some(Need::Uncommitted),
            Wants::OpenPr => Some(Need::UnpushedBranch),
            // No flow answers this: it is finished in preflight, which offers
            // continue and abort where the person is already looking.
            Wants::Resolve => None,
            Wants::Merge | Wants::Attention | Wants::Wait => Some(Need::OpenPullRequest),
            Wants::Release => Some(Need::Release),
            Wants::Nothing => None,
        }
    }
}

#[derive(Clone, PartialEq, Debug)]
pub struct RepoStatus {
    pub branch: String,
    pub changes: usize,
    pub forge: Forge,
    /// The PR for the checked-out branch specifically — what the "Commit /
    /// Review" affordance and the top-of-column `PrCard` react to.
    pub pr: Option<PrBrief>,
    /// Every open PR on the repository, branch-independent — what lets the
    /// sidebar offer a choice of which one to review, not just the one HEAD
    /// happens to point at right now.
    pub prs: Vec<PrBrief>,
    /// Set when the call behind `prs` actually failed (rate limit, network,
    /// auth) rather than the repository genuinely having none — an empty
    /// `prs` alone cannot tell those two apart, and showing "clean" for a
    /// check that never happened is worse than showing nothing.
    pub prs_error: Option<String>,
    /// Commits on this branch not yet on its upstream, and vice versa. Both
    /// `0` whenever there's nothing to report — no upstream configured, or
    /// the branch is caught up — not just "unpushed work exists".
    pub ahead: usize,
    pub behind: usize,
    /// Commits on the checked-out branch that the base branch does not have.
    ///
    /// Distinct from `ahead`, which compares against the *upstream* — a branch
    /// that was never pushed has no upstream, so `ahead` is 0 while the work
    /// plainly exists. That gap is why a branch with a commit and no pull
    /// request used to report "nothing to do".
    pub unmerged: usize,
    /// Merged work the last tag does not reach.
    pub release: ReleaseState,
    /// A git operation that stopped part-way, if any.
    pub in_progress: Option<git::InProgress>,
}

impl RepoStatus {
    /// What a flow's tab counts, given the needs the flow says it handles —
    /// as `(on the tab, on hover)`. Keyed on the declared needs rather than
    /// the flow's name, so a flow called anything shows what it would act on.
    /// `None` when there is nothing to count.
    pub fn count_for(&self, handles: &[String]) -> Option<(String, String)> {
        let has = |need: Need| handles.iter().any(|h| h == need.key());
        let plural =
            |n: usize, one: &str, many: &str| format!("{n} {}", if n == 1 { one } else { many });
        if has(Need::Uncommitted) && self.changes > 0 {
            return Some((
                plural(self.changes, "file", "files"),
                format!(
                    "{} to commit",
                    plural(self.changes, "changed file", "changed files")
                ),
            ));
        }
        if has(Need::UnpushedBranch) && self.unmerged > 0 && self.pr.is_none() {
            return Some((
                plural(self.unmerged, "commit", "commits"),
                format!(
                    "{} on {} with no pull request yet",
                    plural(self.unmerged, "commit", "commits"),
                    self.branch
                ),
            ));
        }
        if has(Need::OpenPullRequest) && !self.prs.is_empty() {
            return Some((
                plural(self.prs.len(), "PR", "PRs"),
                format!(
                    "{} to review",
                    plural(self.prs.len(), "open pull request", "open pull requests")
                ),
            ));
        }
        if has(Need::Release) && self.release.due() {
            let since = self.release.since();
            let not_yet = if self.release.since_shipped {
                "not deployed yet"
            } else {
                "not released yet"
            };
            return Some((
                plural(self.release.commits, "commit", "commits"),
                format!(
                    "{} since {since}, {not_yet}",
                    plural(self.release.commits, "commit", "commits")
                ),
            ));
        }
        None
    }

    /// Uncommitted work outranks a pull request that is merely running its
    /// checks, but never outranks one that is ready for a decision — finishing
    /// something beats starting something.
    /// The most urgent thing this repository needs, across everything it could
    /// need — not just whatever the flow on screen happens to cover.
    ///
    /// Every candidate is collected and the ranking picks the winner, rather
    /// than the first arm of a match short-circuiting the rest. That
    /// distinction is the bug this replaced: an open pull request whose checks
    /// were still running reported "waiting", and a release sitting behind it
    /// was never even considered.
    pub fn wants(&self) -> Wants {
        // Nothing else is worth reporting while git is mid-operation: every
        // other answer would be advice you cannot act on.
        if self.in_progress.is_some() {
            return Wants::Resolve;
        }

        let acting = self.pr_to_act_on();
        // Someone else's pull request — one the checked-out branch does not
        // own — never outranks your own uncommitted work. In a shared
        // repository there is nearly always one open, and letting it win
        // meant a dirty tree was never offered a commit at all.
        let from_pr = acting
            .filter(|_| self.pr.is_some() || self.changes == 0)
            .and_then(|pr| match pr.checks {
                Checks::Passing | Checks::Unknown => Some(Wants::Merge),
                Checks::Failing => Some(Wants::Attention),
                // Nothing to decide yet — but that is not the same as nothing
                // to do, so this contributes no candidate rather than winning.
                Checks::Pending => None,
            });

        [
            from_pr,
            (self.changes > 0).then_some(Wants::Commit),
            // Only without a pull request: with one, the commits are already
            // proposed and the decision is the pull request's.
            (self.pr.is_none() && self.unmerged > 0).then_some(Wants::OpenPr),
            self.release.due().then_some(Wants::Release),
            acting.is_some().then_some(Wants::Wait),
        ]
        .into_iter()
        .flatten()
        .min()
        .unwrap_or(Wants::Nothing)
    }

    /// The pull request this repository most needs a decision on, if any.
    ///
    /// The checked-out branch's own comes first — that is the one being worked
    /// on. Falling back to the rest is not a nicety, it is the difference
    /// between a working chain and a dead one: a commit flow that ends in
    /// `sync` checks the base branch back out, so seconds after opening a pull
    /// request `self.pr` is `None` and the repository it just worked on reads
    /// as having nothing to do. `prs` is what the forge says is actually open,
    /// regardless of where HEAD happens to be pointing.
    pub fn pr_to_act_on(&self) -> Option<&PrBrief> {
        if let Some(pr) = &self.pr {
            return Some(pr);
        }
        // Most actionable first: one that can be decided now beats one that is
        // red, which beats one nothing can be said about yet.
        self.prs.iter().min_by_key(|pr| match pr.checks {
            Checks::Passing | Checks::Unknown => 0,
            Checks::Failing => 1,
            Checks::Pending => 2,
        })
    }

    /// The open pull request to move on to once `reviewed` has been through
    /// the review flow, ranked the same way as `pr_to_act_on`. `None` when it
    /// was the only one.
    pub fn next_to_review(&self, reviewed: &str) -> Option<String> {
        self.pr
            .iter()
            .chain(self.prs.iter())
            .filter(|pr| pr.number != reviewed)
            .min_by_key(|pr| match pr.checks {
                Checks::Passing | Checks::Unknown => 0,
                Checks::Failing => 1,
                Checks::Pending => 2,
            })
            .map(|pr| pr.number.clone())
    }

    /// The pull request to open a review on, when nothing else has said.
    pub fn default_pr(&self) -> String {
        self.pr_to_act_on()
            .map(|pr| pr.number.clone())
            .unwrap_or_default()
    }

    /// One line for the sidebar.
    pub fn summary(&self) -> String {
        match (&self.pr, self.changes) {
            (Some(pr), _) if pr.files > 0 => format!("#{} · {}f", pr.number, pr.files),
            (Some(pr), _) => format!("#{}", pr.number),
            (None, 0) if self.unmerged > 0 => format!("{} ahead", self.unmerged),
            (None, 0) => String::new(),
            (None, n) => n.to_string(),
        }
    }
}

impl Need {
    pub const ALL: [Need; 4] = [
        Need::Uncommitted,
        Need::UnpushedBranch,
        Need::OpenPullRequest,
        Need::Release,
    ];

    /// Stable identifier for the flow file.
    pub fn key(self) -> &'static str {
        match self {
            Need::Uncommitted => "uncommitted",
            Need::UnpushedBranch => "unpushed_branch",
            Need::OpenPullRequest => "open_pull_request",
            Need::Release => "release",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Need::Uncommitted => "Uncommitted changes",
            Need::UnpushedBranch => "A branch with no pull request",
            Need::OpenPullRequest => "An open pull request",
            Need::Release => "Merged work not yet released",
        }
    }

    /// Used by the round-trip test that keeps `key` and `ALL` in step.
    #[cfg(test)]
    pub fn from_key(key: &str) -> Option<Need> {
        Need::ALL.into_iter().find(|n| n.key() == key)
    }
}

pub fn affordance(
    flow: FlowKind,
    status: Option<&RepoStatus>,
    probing: bool,
    already_ran: bool,
    selected_pr: &str,
    problems: &[String],
) -> Affordance {
    // A flow that does not validate is never handed to the executor, so this
    // outranks everything below it: whatever the repository has waiting, this
    // flow cannot be the thing that acts on it. Said here rather than at the
    // button so it is one rule with the rest of them, and tested with them.
    if let Some(first) = problems.first() {
        let reason = match problems.len() {
            1 => format!("{first} Fix it in Setup."),
            n => format!("{first} And {} more. Fix them in Setup.", n - 1),
        };
        return Affordance::stop("Flow is broken", reason);
    }

    let Some(status) = status else {
        return if probing {
            Affordance::stop("Checking…", "Still reading this repository")
        } else {
            // The probe failed or has not run; do not stand in the way.
            Affordance::go("Start run")
        };
    };

    let again = |label: String| {
        if already_ran {
            format!("{label} again")
        } else {
            label
        }
    };

    match flow {
        FlowKind::Commit => match (status.changes, status.unmerged) {
            // Clean tree, but commits sitting on a branch with no pull
            // request: the flow can still push them and propose them.
            (0, 0) => Affordance::stop(
                "Nothing to commit",
                "The working tree is clean and the branch has nothing the base \
                 branch does not — there is nothing for this flow to do",
            ),
            (0, 1) => Affordance::go(again("Push 1 commit & open PR".into())),
            (0, n) => Affordance::go(again(format!("Push {n} commits & open PR"))),
            (1, _) => Affordance::go(again("Commit 1 file".into())),
            (n, _) => Affordance::go(again(format!("Commit {n} files"))),
        },

        // A review that has run is not offered again: merged, the pull
        // request is gone; declined, reviewing the same diff twice says
        // nothing new. The workspace moves on to the next open one when there
        // is one, so this is what is left when there is not.
        FlowKind::Review if already_ran => {
            let reviewed = match (selected_pr, &status.pr) {
                ("", Some(pr)) => pr.number.as_str(),
                (picked, _) => picked,
            };
            match status.next_to_review(reviewed) {
                None => Affordance::stop(
                    "Nothing else to review",
                    "No other pull request is open in this repository",
                ),
                Some(next) => Affordance::stop(
                    "Reviewed",
                    format!("Pick #{next} from the list to review it next"),
                ),
            }
        }
        // A PR picked from the sidebar's list always enables the run — it
        // does not have to be the one for the checked-out branch. Only fall
        // back to "does the checked-out branch have one" when nothing was
        // explicitly picked, which keeps the old single-PR behaviour intact.
        FlowKind::Review if !selected_pr.is_empty() => {
            Affordance::go(format!("Review #{selected_pr}"))
        }
        FlowKind::Review => match &status.pr {
            None => Affordance::stop(
                "No pull request",
                format!("No open pull request for `{}`", status.branch),
            ),
            Some(pr) => Affordance::go(format!("Review #{}", pr.number)),
        },
        FlowKind::Other => Affordance::go(again("Start run".into())),
    }
}
