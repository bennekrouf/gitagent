//! Anonymous usage statistics — GitAgent's vocabulary on top of
//! `desktop_kit::telemetry`, which owns consent, the queue and sending.
//!
//! What GitAgent adds is what only it can say: how a flow run ended, how an
//! approval was decided, and the first run that worked. Every value is mapped
//! here to a fixed tag, so there is no way to hand the kit a path, a
//! repository name, a remote URL, a branch, a commit message or an error
//! string.
//!
//! `GITAGENT_NO_TELEMETRY` still switches it all off: the kit honours each
//! app's `<ID>_NO_TELEMETRY` next to `DO_NOT_TRACK` and `DISABLE_UPDATE_CHECK`.

use desktop_kit::telemetry::{self as kit, Value};

use crate::services::flowdef::FlowBook;
use crate::services::forge::Forge;
use crate::services::graph::NodeStatus;
use crate::services::llm::ProviderKind;
use crate::services::store;

pub use kit::{
    active, available, flush, flush_forever, mark_informed, set_consent, shared, should_ask,
};

/// Before anything is recorded, and after `services::env::adopt_login_env`:
/// the opt-outs are already in the process environment, so the kit need not
/// ask the login shell again. Also moves the answer and identity GitAgent kept
/// in its own data folder before it used the kit, so nobody is asked twice
/// and no install is counted twice.
pub fn init() {
    kit::login_env_adopted();
    kit::adopt_legacy(&store::data_dir(), &[("activated", FIRST_FLOW_COMPLETED)]);
}

const FIRST_FLOW_COMPLETED: &str = "first_flow_completed";

/// How a run ended. Four values, on purpose: the question is "did it work",
/// not "what happened at node 7".
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// At least one step did its work and nothing failed or was declined.
    Done,
    Failed,
    /// A person declined an approval.
    Rejected,
    /// Every step found nothing to do.
    Nothing,
}

impl Outcome {
    fn tag(self) -> &'static str {
        match self {
            Outcome::Done => "done",
            Outcome::Failed => "failed",
            Outcome::Rejected => "rejected",
            Outcome::Nothing => "nothing",
        }
    }
}

/// The one word for how a finished run went, from where every step ended up.
/// A failure outranks a decline, and a decline outranks success: a run that
/// committed and then failed to push did not work.
pub fn outcome_of(statuses: &[NodeStatus]) -> Outcome {
    let any = |wanted: NodeStatus| statuses.contains(&wanted);
    if any(NodeStatus::Failed) {
        Outcome::Failed
    } else if any(NodeStatus::Rejected) {
        Outcome::Rejected
    } else if any(NodeStatus::Done) {
        Outcome::Done
    } else {
        Outcome::Nothing
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
    /// The process started. The denominator for everything else.
    AppStarted,
    /// One drive of one flow ended.
    FlowFinished {
        flow: &'static str,
        outcome: Outcome,
        forge: &'static str,
        provider: &'static str,
    },
    ApprovalDecided {
        approved: bool,
        trusted: bool,
    },
    LicenceActivated,
    UpdateOffered {
        to: String,
    },
    UpdateClicked {
        to: String,
    },
}

impl Event {
    /// Builds a `FlowFinished`, mapping everything that could identify the
    /// user's own setup down to a fixed tag: a flow the user wrote or renamed
    /// is `custom`, not its name.
    pub fn flow_finished(
        flow_id: &str,
        outcome: Outcome,
        forge: &Forge,
        provider: ProviderKind,
    ) -> Self {
        Event::FlowFinished {
            flow: flow_tag(flow_id),
            outcome,
            forge: forge_tag(forge),
            provider: provider_tag(provider),
        }
    }

    fn into_kit(self) -> kit::Event {
        let custom = |name, props| kit::Event::Custom { name, props };
        match self {
            Event::AppStarted => kit::Event::AppStarted,
            Event::FlowFinished {
                flow,
                outcome,
                forge,
                provider,
            } => custom(
                "flow_finished",
                vec![
                    ("flow", Value::Tag(flow)),
                    ("outcome", Value::Tag(outcome.tag())),
                    ("forge", Value::Tag(forge)),
                    ("provider", Value::Tag(provider)),
                ],
            ),
            Event::ApprovalDecided { approved, trusted } => custom(
                "approval_decided",
                vec![
                    ("approved", Value::Flag(approved)),
                    ("by", Value::Tag(if trusted { "trusted" } else { "person" })),
                ],
            ),
            Event::LicenceActivated => custom("licence_activated", Vec::new()),
            Event::UpdateOffered { to } => kit::Event::UpdateOffered { to },
            Event::UpdateClicked { to } => kit::Event::UpdateClicked { to },
        }
    }
}

/// Note that something happened. Synchronous, cheap, and silent about every
/// way it can fail; safe to call from anywhere, including a hot path.
pub fn record(event: Event) {
    // The first run that actually completes is the moment a download became a
    // user; the kit keeps it to once per install.
    let activated = matches!(
        event,
        Event::FlowFinished {
            outcome: Outcome::Done,
            ..
        }
    );
    kit::record(event.into_kit());
    if activated {
        kit::record(kit::Event::Milestone(FIRST_FLOW_COMPLETED));
    }
}

/// Only the flows the app ships are named. Anything else — a flow the person
/// built or renamed — could carry their own vocabulary, so it is `custom`.
fn flow_tag(id: &str) -> &'static str {
    const SHIPPED: &[&str] = &["commit_and_pr", "review_and_merge", "release"];
    if !FlowBook::defaults().flows.iter().any(|f| f.id == id) {
        return "custom";
    }
    SHIPPED
        .iter()
        .copied()
        .find(|s| *s == id)
        .unwrap_or("custom")
}

/// `Unsupported` carries the remote's URL or host; it collapses to `other` so
/// that string can never travel.
pub fn forge_tag(forge: &Forge) -> &'static str {
    match forge {
        Forge::GitHub => "github",
        Forge::AzureDevOps => "azure",
        Forge::Unsupported(_) => "other",
        Forge::None => "none",
    }
}

fn provider_tag(kind: ProviderKind) -> &'static str {
    match kind {
        ProviderKind::Ollama => "local",
        ProviderKind::Remote => "remote",
        ProviderKind::Off => "off",
    }
}

#[cfg(test)]
mod tests;
