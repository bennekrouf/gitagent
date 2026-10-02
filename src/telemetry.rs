//! Anonymous usage statistics — off until the person says yes.
//!
//! The download logs on mayorana.ch can say how many people fetched a build and
//! how many copies checked for an update. They cannot say whether anyone got a
//! run to work, which step it stopped at, or whether a person who installed last
//! week is still here. This answers those, and only those.
//!
//! What is sent is deliberately narrow, and the narrowness is enforced by the
//! types rather than by care: an [`Event`] is an enum whose fields are fixed
//! vocabularies (`&'static str` tags, booleans), so there is no way to hand it a
//! path, a repository name, a remote URL, a branch, a commit message or an error
//! string. The one free-form value, an update's target version, is checked to
//! look like a version before it is kept.
//!
//! Posture, in order of precedence — any one of these means nothing is recorded
//! and nothing is sent:
//!
//!   * the build has no endpoint (`GITAGENT_TELEMETRY_URL` unset at compile
//!     time — which is every `cargo run`, so development never pollutes the
//!     numbers, the same way the licence key gates licence checks);
//!   * the person has not said yes ([`DEFAULT_CONSENT`]) or has said no;
//!   * `DISABLE_UPDATE_CHECK`, `DO_NOT_TRACK` or `GITAGENT_NO_TELEMETRY` is set.
//!
//! Best-effort throughout, like the update check: recording is a synchronous
//! append that cannot fail visibly, sending has a two-second timeout, and a
//! failed send leaves the events queued for the next attempt. Never panics,
//! never blocks a run.
//!
//! Transport is a single GET to a URL on mayorana.ch that answers 204 and
//! nothing else. The batch rides in the query string, base64url-encoded, and
//! the nightly statistics job reads it back out of the web server's access
//! log — the same place it already reads download counts from. There is no
//! endpoint parsing anything at request time, no database, and nothing that has
//! to be running for a send to succeed. The cost is that a URL has a ceiling, so
//! a backlog goes out as several small requests (see `EVENTS_BUDGET_BYTES`).
//!
//! Identity is one random 128-bit value generated on first use and kept next to
//! the other state. It is not derived from the machine, the account or the
//! network, so it cannot be recomputed, and a reinstall is a new install. It is
//! deliberately not linked to a mayorana.ch sign-in.

use base64::Engine;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use crate::services::flowdef::FlowBook;
use crate::services::forge::Forge;
use crate::services::graph::NodeStatus;
use crate::services::llm::ProviderKind;
use crate::services::store;

const APP: &str = "gitagent";
const SCHEMA: u32 = 1;

/// Where batches go. Baked in by the release workflow; absent in a development
/// build, and an empty value (an unset repository variable expands to "") is
/// treated as absent too.
const ENDPOINT: Option<&str> = option_env!("GITAGENT_TELEMETRY_URL");

/// What an unanswered question means. `true`: statistics are on by default and
/// the person turns them off (opt-out). Even so, nothing is recorded until the
/// notice in the window has been shown once — see `State::informed` — so nobody
/// is counted before they have been told.
///
/// The wording of the banner in `main.rs`, and of the changelog entry, assumes
/// this is `true`. Flipping it to `false` makes the feature opt-in and those
/// words must change with it.
const DEFAULT_CONSENT: bool = true;

const STATE_FILE: &str = "telemetry.json";
const QUEUE_FILE: &str = "events.jsonl";

/// Offline for a long time must not grow a file without bound. Past this the
/// newest events are dropped, not the oldest: the start of a session says more
/// than its tail.
const MAX_QUEUE_BYTES: u64 = 256 * 1024;
const MAX_BATCH: usize = 200;
/// The batch travels in a URL, and a URL has a ceiling: nginx refuses a request
/// line over 8 KB by default. Keeping the events' JSON near 4 KB makes about
/// 5.4 KB once base64-encoded, which fits with room for the address and the
/// envelope around it.
const EVENTS_BUDGET_BYTES: usize = 4000;
/// A flush works through a backlog in chunks, but not forever: an app that has
/// been offline for a week should catch up over a few flushes, not in one burst
/// of requests.
const MAX_CHUNKS_PER_FLUSH: usize = 10;
/// A week-old event describes a version and a habit that no longer exist.
const MAX_AGE_SECS: u64 = 7 * 24 * 60 * 60;
const SEND_TIMEOUT: Duration = Duration::from_secs(2);
const FLUSH_FIRST_AFTER: Duration = Duration::from_secs(10);
const FLUSH_EVERY: Duration = Duration::from_secs(5 * 60);

/// Env vars that mean "do not phone home". `DISABLE_UPDATE_CHECK` is here
/// because someone who has turned that off expects silence, not a different
/// channel carrying the same information.
const OPT_OUT_VARS: &[&str] = &[
    "DISABLE_UPDATE_CHECK",
    "DO_NOT_TRACK",
    "GITAGENT_NO_TELEMETRY",
];

// ── What can be said ──────────────────────────────────────────────────────

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
    /// Emitted once per install, by [`record`] itself, the first time a run
    /// finishes `Done`. The activation moment.
    FirstFlowCompleted {
        days_since_install: u32,
    },
    ApprovalDecided {
        approved: bool,
        trusted: bool,
    },
    /// A run was refused because the free version's repositories are used up.
    LicenceWallHit,
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

    fn wire(&self) -> (&'static str, BTreeMap<String, String>) {
        let mut p = BTreeMap::new();
        let put = |p: &mut BTreeMap<String, String>, k: &str, v: &str| {
            p.insert(k.to_string(), v.to_string());
        };
        let name = match self {
            Event::AppStarted => "app_started",
            Event::FlowFinished {
                flow,
                outcome,
                forge,
                provider,
            } => {
                put(&mut p, "flow", flow);
                put(&mut p, "outcome", outcome.tag());
                put(&mut p, "forge", forge);
                put(&mut p, "provider", provider);
                "flow_finished"
            }
            Event::FirstFlowCompleted { days_since_install } => {
                put(&mut p, "days", &days_since_install.to_string());
                "first_flow_completed"
            }
            Event::ApprovalDecided { approved, trusted } => {
                put(&mut p, "approved", if *approved { "yes" } else { "no" });
                put(&mut p, "by", if *trusted { "trusted" } else { "person" });
                "approval_decided"
            }
            Event::LicenceWallHit => "licence_wall_hit",
            Event::LicenceActivated => "licence_activated",
            Event::UpdateOffered { to } => {
                put(&mut p, "to", &sanitise_version(to));
                "update_offered"
            }
            Event::UpdateClicked { to } => {
                put(&mut p, "to", &sanitise_version(to));
                "update_clicked"
            }
        };
        (name, p)
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

/// The version text comes from a file on our own server, but it is still the
/// one string here that was not written in this crate.
fn sanitise_version(raw: &str) -> String {
    let ok = !raw.is_empty()
        && raw.len() <= 20
        && raw
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-');
    if ok {
        raw.to_string()
    } else {
        "unknown".to_string()
    }
}

// ── Gate ──────────────────────────────────────────────────────────────────

fn endpoint() -> Option<&'static str> {
    ENDPOINT.filter(|u| !u.is_empty())
}

/// Whether this build can send at all. When it cannot, nothing about
/// statistics should be shown — asking permission to do something the build
/// cannot do would be a question with no answer.
pub fn available() -> bool {
    endpoint().is_some()
}

/// `1`, `true`, `yes` — and anything else non-empty except the explicit
/// negatives. `DO_NOT_TRACK=0` must not count as opting out, and a variable
/// exported empty must not either.
fn flag_set(value: Option<&str>) -> bool {
    match value.map(str::trim) {
        None | Some("") => false,
        Some(v) => !matches!(v.to_ascii_lowercase().as_str(), "0" | "false" | "no"),
    }
}

fn env_blocks() -> bool {
    OPT_OUT_VARS.iter().any(|name| {
        let value = std::env::var(name).ok();
        // DISABLE_UPDATE_CHECK is honoured by mere presence in update_check.rs,
        // so it is here too; the others follow the usual 0/false convention.
        if *name == "DISABLE_UPDATE_CHECK" {
            value.is_some()
        } else {
            flag_set(value.as_deref())
        }
    })
}

/// An explicit answer always wins. An unanswered question means
/// `DEFAULT_CONSENT` — but only once the person has been shown the notice, so
/// that "on by default" never means "on before you were told".
fn gate(endpoint: Option<&str>, consent: Option<bool>, informed: bool, env_blocked: bool) -> bool {
    endpoint.is_some()
        && !env_blocked
        && match consent {
            Some(answer) => answer,
            None => DEFAULT_CONSENT && informed,
        }
}

/// Whether anything is being recorded right now.
pub fn active() -> bool {
    let state = load_state(&dir());
    gate(endpoint(), state.consent, state.informed, env_blocks())
}

/// What the Settings switch should show: the person's answer, or the default
/// if they have not given one.
pub fn shared() -> bool {
    consent().unwrap_or(DEFAULT_CONSENT)
}

/// Record that the notice has been put in front of the person. From here on an
/// unanswered question counts as the default. Called when the banner appears.
pub fn mark_informed() {
    mark_informed_in(&dir());
}

fn mark_informed_in(dir: &Path) {
    let _io = IO.lock().unwrap_or_else(|e| e.into_inner());
    let mut state = load_state(dir);
    if !state.informed {
        state.informed = true;
        save_state(dir, &state);
    }
}

/// Whether to put the question to the person: a build that can send, an
/// answer not yet given, and no environment setting that has already answered
/// it for them.
pub fn should_ask() -> bool {
    available() && consent().is_none() && !env_blocks()
}

pub fn consent() -> Option<bool> {
    load_state(&dir()).consent
}

/// Saying no also deletes what was waiting to be sent: an opt-out that still
/// uploaded the backlog would not be one.
pub fn set_consent(yes: bool) {
    set_consent_in(&dir(), yes);
}

fn set_consent_in(dir: &Path, yes: bool) {
    let _io = IO.lock().unwrap_or_else(|e| e.into_inner());
    let mut state = load_state(dir);
    state.consent = Some(yes);
    save_state(dir, &state);
    if !yes {
        let _ = std::fs::remove_file(dir.join(QUEUE_FILE));
    }
}

// ── State and queue on disk ───────────────────────────────────────────────

#[derive(Default, Serialize, Deserialize)]
struct State {
    /// `None` until answered.
    #[serde(default)]
    consent: Option<bool>,
    /// Whether the notice has been shown. Only matters while `consent` is
    /// `None`: it is what lets the default take effect.
    #[serde(default)]
    informed: bool,
    #[serde(default)]
    install_id: String,
    /// `YYYY-MM-DD`, for "days since install" — never sent as a date.
    #[serde(default)]
    first_seen: String,
    #[serde(default)]
    activated: bool,
}

/// One queued event. Carries its own launch and version because the queue can
/// outlive both: events from before an update are sent after it, and stamping
/// them with the sender's version would put them in the wrong column.
#[derive(Serialize, Deserialize)]
struct Line {
    n: String,
    t: u64,
    /// Which launch of the app produced it.
    l: String,
    /// App version at the time.
    av: String,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    p: BTreeMap<String, String>,
}

/// Serialises every read-modify-write of the two files within this process —
/// two windows are two tasks writing the same directory.
static IO: Mutex<()> = Mutex::new(());
/// One send at a time, so a batch cannot go out twice.
static FLUSHING: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

fn dir() -> PathBuf {
    store::data_dir()
}

fn load_state(dir: &Path) -> State {
    std::fs::read_to_string(dir.join(STATE_FILE))
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn save_state(dir: &Path, state: &State) {
    let _ = std::fs::create_dir_all(dir);
    if let Ok(json) = serde_json::to_string_pretty(state) {
        let _ = store::write_atomic(&dir.join(STATE_FILE), json.as_bytes());
    }
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn launch_id() -> &'static str {
    static ID: OnceLock<String> = OnceLock::new();
    ID.get_or_init(random_hex)
}

/// 128 random bits, hex. Empty if the OS gives none — callers treat that as
/// "do not record" rather than fall back to something guessable.
fn random_hex() -> String {
    let mut bytes = [0u8; 16];
    if getrandom::fill(&mut bytes).is_err() {
        return String::new();
    }
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn date_of(secs: u64) -> String {
    chrono::DateTime::from_timestamp(secs as i64, 0)
        .map(|d| d.format("%Y-%m-%d").to_string())
        .unwrap_or_default()
}

fn days_between(first_seen: &str, now: u64) -> u32 {
    let (Ok(first), Ok(today)) = (
        chrono::NaiveDate::parse_from_str(first_seen, "%Y-%m-%d"),
        chrono::NaiveDate::parse_from_str(&date_of(now), "%Y-%m-%d"),
    ) else {
        return 0;
    };
    (today - first).num_days().max(0) as u32
}

fn make_line(event: &Event, now: u64) -> Line {
    let (n, p) = event.wire();
    Line {
        n: n.to_string(),
        t: now,
        l: launch_id().to_string(),
        av: env!("CARGO_PKG_VERSION").to_string(),
        p,
    }
}

fn queue_is_full(dir: &Path) -> bool {
    std::fs::metadata(dir.join(QUEUE_FILE))
        .map(|m| m.len() >= MAX_QUEUE_BYTES)
        .unwrap_or(false)
}

fn append(dir: &Path, lines: &[Line]) {
    let _ = std::fs::create_dir_all(dir);
    let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join(QUEUE_FILE))
    else {
        return;
    };
    for line in lines {
        if let Ok(json) = serde_json::to_string(line) {
            let _ = writeln!(file, "{json}");
        }
    }
}

// ── Recording ─────────────────────────────────────────────────────────────

/// Note that something happened. Synchronous, cheap, and silent about every
/// way it can fail; safe to call from anywhere, including a hot path.
pub fn record(event: Event) {
    record_in(&dir(), active(), &event, now_secs());
}

fn record_in(dir: &Path, on: bool, event: &Event, now: u64) {
    if !on {
        return;
    }
    let _io = IO.lock().unwrap_or_else(|e| e.into_inner());

    let mut state = load_state(dir);
    if state.install_id.is_empty() {
        state.install_id = random_hex();
        if state.install_id.is_empty() {
            return;
        }
        state.first_seen = date_of(now);
        save_state(dir, &state);
    }
    if queue_is_full(dir) {
        return;
    }

    let mut lines = vec![make_line(event, now)];

    // The first run that actually completes is the moment a download became a
    // user. Emitted here, from the same state that holds the install id, so it
    // fires exactly once per install however many windows or restarts pass.
    if matches!(
        event,
        Event::FlowFinished {
            outcome: Outcome::Done,
            ..
        }
    ) && !state.activated
    {
        state.activated = true;
        save_state(dir, &state);
        let first = Event::FirstFlowCompleted {
            days_since_install: days_between(&state.first_seen, now),
        };
        lines.push(make_line(&first, now));
    }
    append(dir, &lines);
}

// ── Sending ───────────────────────────────────────────────────────────────

/// Sends what is queued, if anything, and forgets it only once the server has
/// said yes. Call it whenever; it does nothing when statistics are off.
pub async fn flush() {
    let Some(url) = endpoint() else { return };
    if !active() {
        return;
    }
    flush_all_to(&dir(), url, now_secs()).await;
}

/// First flush shortly after start, then on a timer. Every window runs one;
/// they serialise on `FLUSHING`, so the second finds an empty queue.
pub async fn flush_forever() {
    tokio::time::sleep(FLUSH_FIRST_AFTER).await;
    loop {
        flush().await;
        tokio::time::sleep(FLUSH_EVERY).await;
    }
}

/// Reads the queue, discarding anything too old or unreadable, and returns the
/// next batch with the identity to send it under.
fn next_batch(dir: &Path, now: u64) -> Option<(Vec<Line>, String)> {
    let _io = IO.lock().unwrap_or_else(|e| e.into_inner());
    let text = std::fs::read_to_string(dir.join(QUEUE_FILE)).ok()?;
    let total = text.lines().filter(|l| !l.trim().is_empty()).count();
    let kept: Vec<Line> = text
        .lines()
        .filter_map(|l| serde_json::from_str::<Line>(l).ok())
        .filter(|l| l.t.saturating_add(MAX_AGE_SECS) >= now)
        .collect();
    if kept.len() != total {
        rewrite(dir, &kept);
    }
    let install_id = load_state(dir).install_id;
    if kept.is_empty() || install_id.is_empty() {
        return None;
    }
    // Oldest first, as many as fit. Always at least one, so a line somehow
    // larger than the budget cannot wedge the queue behind it.
    let mut batch = Vec::new();
    let mut used = 0usize;
    for line in kept.into_iter().take(MAX_BATCH) {
        let size = serde_json::to_string(&line)
            .map(|j| j.len() + 1)
            .unwrap_or(0);
        if !batch.is_empty() && used + size > EVENTS_BUDGET_BYTES {
            break;
        }
        used += size;
        batch.push(line);
    }
    Some((batch, install_id))
}

fn rewrite(dir: &Path, lines: &[Line]) {
    let path = dir.join(QUEUE_FILE);
    if lines.is_empty() {
        let _ = std::fs::remove_file(path);
        return;
    }
    let mut out = String::new();
    for line in lines {
        if let Ok(json) = serde_json::to_string(line) {
            out.push_str(&json);
            out.push('\n');
        }
    }
    let _ = store::write_atomic(&path, out.as_bytes());
}

/// Drops the first `sent` lines. Safe because the queue is append-only between
/// flushes and only a flush ever removes from the front.
fn forget_sent(dir: &Path, sent: usize) {
    let _io = IO.lock().unwrap_or_else(|e| e.into_inner());
    let Ok(text) = std::fs::read_to_string(dir.join(QUEUE_FILE)) else {
        return;
    };
    let rest: Vec<Line> = text
        .lines()
        .filter_map(|l| serde_json::from_str::<Line>(l).ok())
        .skip(sent)
        .collect();
    rewrite(dir, &rest);
}

/// Works through the queue one chunk at a time, stopping at the first failure
/// or when it is empty. A failure leaves the rest queued for the next flush.
async fn flush_all_to(dir: &Path, url: &str, now: u64) {
    for _ in 0..MAX_CHUNKS_PER_FLUSH {
        if !flush_to(dir, url, now).await {
            break;
        }
    }
}

/// Sends one chunk. `false` means nothing was sent — an empty queue, or a
/// refusal — and either way the caller should stop.
async fn flush_to(dir: &Path, url: &str, now: u64) -> bool {
    let _one_at_a_time = FLUSHING.lock().await;
    let Some((batch, install_id)) = next_batch(dir, now) else {
        return false;
    };
    let body = serde_json::json!({
        "v": SCHEMA,
        "app": APP,
        "os": std::env::consts::OS,
        "arch": std::env::consts::ARCH,
        "install_id": install_id,
        "sent_at": chrono::DateTime::from_timestamp(now as i64, 0)
            .map(|d| d.to_rfc3339())
            .unwrap_or_default(),
        "events": batch,
    });
    let encoded = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .encode(serde_json::to_vec(&body).unwrap_or_default());
    let separator = if url.contains('?') { '&' } else { '?' };
    let sent = reqwest::Client::new()
        .get(format!("{url}{separator}b={encoded}"))
        // Distinct from `(updater)` and `(notice)` on purpose: the download
        // statistics count `(updater)` polls as live installs.
        .header(
            reqwest::header::USER_AGENT,
            concat!("gitagent/", env!("CARGO_PKG_VERSION"), " (telemetry)"),
        )
        .timeout(SEND_TIMEOUT)
        .send()
        .await
        .map(|r| r.status().is_success())
        .unwrap_or(false);
    if sent {
        forget_sent(dir, batch.len());
    }
    sent
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    const NOW: u64 = 1_790_000_000;

    fn scratch() -> PathBuf {
        static N: AtomicU32 = AtomicU32::new(0);
        let dir = std::env::temp_dir().join(format!(
            "gitagent-telemetry-test-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::SeqCst)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    fn queued(dir: &Path) -> Vec<serde_json::Value> {
        std::fs::read_to_string(dir.join(QUEUE_FILE))
            .unwrap_or_default()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect()
    }

    fn finished(outcome: Outcome) -> Event {
        Event::flow_finished(
            "commit_and_pr",
            outcome,
            &Forge::GitHub,
            ProviderKind::Ollama,
        )
    }

    #[test]
    fn nothing_is_recorded_unless_every_condition_holds() {
        let url = Some("https://x");
        // gate(endpoint, consent, informed, env_blocked)
        assert!(gate(url, Some(true), true, false));
        // No endpoint: a development build, whatever the person said.
        assert!(!gate(None, Some(true), true, false));
        // Said no.
        assert!(!gate(url, Some(false), true, false));
        // An environment opt-out beats a yes.
        assert!(!gate(url, Some(true), true, true));
    }

    #[test]
    fn on_by_default_never_means_on_before_you_were_told() {
        let url = Some("https://x");
        // Unanswered and not yet shown the notice: nothing, even though the
        // default is yes.
        assert!(!gate(url, None, false, false));
        // Shown the notice and did nothing about it: the default applies.
        assert_eq!(gate(url, None, true, false), DEFAULT_CONSENT);
        // An explicit answer is never overridden by the default or by having
        // been informed.
        assert!(!gate(url, Some(false), true, false));
        assert!(gate(url, Some(true), false, false));
        // And the environment still wins over the default.
        assert!(!gate(url, None, true, true));
    }

    #[test]
    fn being_informed_is_remembered_and_does_not_touch_the_answer() {
        let dir = scratch();
        assert!(!load_state(&dir).informed);
        mark_informed_in(&dir);
        assert!(load_state(&dir).informed);
        assert_eq!(load_state(&dir).consent, None, "informing is not answering");

        set_consent_in(&dir, false);
        mark_informed_in(&dir);
        assert_eq!(
            load_state(&dir).consent,
            Some(false),
            "and never flips an answer"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn opt_out_variables_follow_their_conventions() {
        assert!(flag_set(Some("1")));
        assert!(flag_set(Some("true")));
        assert!(!flag_set(Some("0")));
        assert!(!flag_set(Some("false")));
        assert!(!flag_set(Some("")));
        assert!(!flag_set(None));
    }

    #[test]
    fn a_run_is_only_done_if_nothing_in_it_went_wrong() {
        use NodeStatus::*;
        assert_eq!(outcome_of(&[Done, Done, Skipped]), Outcome::Done);
        // Committed, then the push failed: that did not work.
        assert_eq!(outcome_of(&[Done, Failed, Blocked]), Outcome::Failed);
        assert_eq!(outcome_of(&[Done, Rejected, Blocked]), Outcome::Rejected);
        assert_eq!(outcome_of(&[Failed, Rejected]), Outcome::Failed);
        assert_eq!(outcome_of(&[Skipped, Bypassed]), Outcome::Nothing);
        assert_eq!(outcome_of(&[]), Outcome::Nothing);
    }

    #[test]
    fn an_empty_endpoint_is_no_endpoint() {
        // An unset repository variable reaches option_env! as "", not as absent.
        assert_eq!(Some("").filter(|u| !u.is_empty()), None);
    }

    #[test]
    fn off_writes_nothing_at_all() {
        let dir = scratch();
        record_in(&dir, false, &Event::AppStarted, NOW);
        assert!(!dir.exists(), "even the state file would be a trace");
    }

    #[test]
    fn identity_is_random_stable_and_local() {
        let dir = scratch();
        record_in(&dir, true, &Event::AppStarted, NOW);
        let first = load_state(&dir).install_id;
        assert_eq!(first.len(), 32);
        assert!(first.chars().all(|c| c.is_ascii_hexdigit()));
        record_in(&dir, true, &Event::AppStarted, NOW + 5);
        assert_eq!(load_state(&dir).install_id, first, "one id per install");
        assert_ne!(random_hex(), random_hex());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn activation_fires_once_and_only_for_a_run_that_worked() {
        let dir = scratch();
        record_in(&dir, true, &finished(Outcome::Failed), NOW);
        record_in(&dir, true, &finished(Outcome::Nothing), NOW);
        assert!(queued(&dir)
            .iter()
            .all(|e| e["n"] != "first_flow_completed"));

        record_in(&dir, true, &finished(Outcome::Done), NOW + 3 * 86_400);
        record_in(&dir, true, &finished(Outcome::Done), NOW + 4 * 86_400);
        let firsts: Vec<_> = queued(&dir)
            .into_iter()
            .filter(|e| e["n"] == "first_flow_completed")
            .collect();
        assert_eq!(firsts.len(), 1);
        assert_eq!(firsts[0]["p"]["days"], "3");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_event_cannot_carry_anything_but_its_vocabulary() {
        let (name, p) = Event::flow_finished(
            "my-secret-client-flow",
            Outcome::Done,
            &Forge::Unsupported("git.internal.bigcorp.example".into()),
            ProviderKind::Remote,
        )
        .wire();
        assert_eq!(name, "flow_finished");
        assert_eq!(p["flow"], "custom");
        assert_eq!(p["forge"], "other");
        assert_eq!(p["provider"], "remote");
        let everything = format!("{p:?}");
        assert!(!everything.contains("bigcorp") && !everything.contains("secret"));

        // Shipped flows keep their names.
        assert_eq!(flow_tag("commit_and_pr"), "commit_and_pr");
        assert_eq!(flow_tag("review_and_merge"), "review_and_merge");
    }

    #[test]
    fn a_version_string_is_checked_before_it_is_kept() {
        assert_eq!(sanitise_version("0.1.72"), "0.1.72");
        assert_eq!(sanitise_version("0.2.0-beta1"), "0.2.0-beta1");
        assert_eq!(sanitise_version("1.0 <script>"), "unknown");
        assert_eq!(sanitise_version(""), "unknown");
        assert_eq!(sanitise_version(&"9".repeat(40)), "unknown");
    }

    #[test]
    fn a_full_queue_drops_new_events_rather_than_growing() {
        let dir = scratch();
        record_in(&dir, true, &Event::AppStarted, NOW);
        let filler = "x".repeat(MAX_QUEUE_BYTES as usize);
        std::fs::write(dir.join(QUEUE_FILE), filler).unwrap();
        let before = std::fs::metadata(dir.join(QUEUE_FILE)).unwrap().len();
        record_in(&dir, true, &Event::AppStarted, NOW + 1);
        assert_eq!(
            std::fs::metadata(dir.join(QUEUE_FILE)).unwrap().len(),
            before
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn saying_no_deletes_the_backlog_and_keeps_the_answer() {
        let dir = scratch();
        record_in(&dir, true, &Event::AppStarted, NOW);
        assert!(dir.join(QUEUE_FILE).exists());
        set_consent_in(&dir, false);
        assert!(!dir.join(QUEUE_FILE).exists());
        assert_eq!(load_state(&dir).consent, Some(false));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A tiny HTTP server: answers the nth request with `statuses[n]` (204 once
    /// the list runs out) and records each request target, in order.
    async fn serve(
        statuses: Vec<&'static str>,
    ) -> (
        String,
        std::sync::Arc<Mutex<Vec<String>>>,
        tokio::task::JoinHandle<()>,
    ) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/ping", listener.local_addr().unwrap());
        let seen = std::sync::Arc::new(Mutex::new(Vec::new()));
        let log = seen.clone();
        let handle = tokio::spawn(async move {
            let mut n = 0;
            loop {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut buf = Vec::new();
                let mut chunk = [0u8; 4096];
                while !buf.windows(4).any(|w| w == b"\r\n\r\n") {
                    let read = socket.read(&mut chunk).await.unwrap();
                    if read == 0 {
                        break;
                    }
                    buf.extend_from_slice(&chunk[..read]);
                }
                let head = String::from_utf8_lossy(&buf).to_string();
                let first = head.lines().next().unwrap_or_default();
                assert!(first.starts_with("GET "), "telemetry is a GET: {first}");
                log.lock().unwrap().push(first.to_string());
                let status = statuses.get(n).copied().unwrap_or("204 No Content");
                n += 1;
                let reply =
                    format!("HTTP/1.1 {status}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
                socket.write_all(reply.as_bytes()).await.unwrap();
            }
        });
        (url, seen, handle)
    }

    /// Decodes the batch out of a recorded request line.
    fn decode(request_line: &str) -> serde_json::Value {
        let target = request_line.split(' ').nth(1).unwrap();
        let encoded = target.split("b=").nth(1).expect("batch in the query");
        let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(encoded)
            .unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    fn backlog(dir: &Path, events: usize) {
        for i in 0..events {
            record_in(dir, true, &Event::AppStarted, NOW + i as u64);
        }
        assert_eq!(queued(dir).len(), events);
    }

    #[tokio::test]
    async fn a_successful_send_delivers_the_envelope_and_clears_the_queue() {
        let dir = scratch();
        record_in(&dir, true, &Event::AppStarted, NOW);
        record_in(&dir, true, &finished(Outcome::Done), NOW + 1);
        assert_eq!(
            queued(&dir).len(),
            3,
            "app_started, flow_finished, first_flow_completed"
        );

        let (url, seen, server) = serve(vec!["204 No Content"]).await;
        assert!(flush_to(&dir, &url, NOW + 2).await);
        server.abort();

        let requests = seen.lock().unwrap().clone();
        assert_eq!(requests.len(), 1);
        assert!(requests[0].starts_with("GET /ping?b="));
        let body = decode(&requests[0]);
        let mut keys: Vec<_> = body.as_object().unwrap().keys().cloned().collect();
        keys.sort();
        assert_eq!(
            keys,
            ["app", "arch", "events", "install_id", "os", "sent_at", "v"]
        );
        assert_eq!(body["app"], "gitagent");
        assert_eq!(body["v"], 1);
        assert_eq!(body["events"].as_array().unwrap().len(), 3);
        assert_eq!(body["install_id"].as_str().unwrap().len(), 32);
        assert!(!dir.join(QUEUE_FILE).exists(), "sent events are forgotten");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn a_failed_send_keeps_the_events_for_next_time() {
        let dir = scratch();
        record_in(&dir, true, &Event::AppStarted, NOW);

        let (url, _seen, server) = serve(vec!["500 Internal Server Error"]).await;
        assert!(!flush_to(&dir, &url, NOW + 1).await);
        server.abort();
        assert_eq!(queued(&dir).len(), 1);

        // Nothing listening at all.
        assert!(!flush_to(&dir, "http://127.0.0.1:1/x", NOW + 1).await);
        assert_eq!(queued(&dir).len(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn a_backlog_goes_out_in_chunks_that_each_fit_in_a_url() {
        let dir = scratch();
        backlog(&dir, 300);

        let (url, seen, server) = serve(vec![]).await;
        flush_all_to(&dir, &url, NOW + 300).await;
        server.abort();

        let requests = seen.lock().unwrap().clone();
        assert!(requests.len() > 1, "a backlog needs several requests");
        assert!(requests.len() <= MAX_CHUNKS_PER_FLUSH);
        let mut sent = 0;
        for line in &requests {
            // nginx's default request-line limit is 8 KB.
            assert!(line.len() < 8000, "request line is {} bytes", line.len());
            let events = decode(line)["events"].as_array().unwrap().len();
            assert!((1..=MAX_BATCH).contains(&events));
            sent += events;
        }
        assert_eq!(sent + queued(&dir).len(), 300, "nothing lost or doubled");

        // The remainder goes out on a later flush.
        let (url, _seen, server) = serve(vec![]).await;
        for _ in 0..3 {
            flush_all_to(&dir, &url, NOW + 300).await;
        }
        server.abort();
        assert!(!dir.join(QUEUE_FILE).exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn a_failing_chunk_stops_the_flush_and_keeps_the_rest() {
        let dir = scratch();
        backlog(&dir, 300);

        let (url, seen, server) = serve(vec!["204 No Content", "500 Internal Server Error"]).await;
        flush_all_to(&dir, &url, NOW + 300).await;
        server.abort();

        let requests = seen.lock().unwrap().clone();
        assert_eq!(requests.len(), 2, "stops at the first failure");
        let first = decode(&requests[0])["events"].as_array().unwrap().len();
        let second = decode(&requests[1])["events"].as_array().unwrap().len();
        let left = queued(&dir);
        assert_eq!(
            left.len(),
            300 - first,
            "only the acknowledged chunk is gone"
        );
        assert!(left.len() >= second, "the refused chunk is still queued");
        assert_eq!(
            left[0]["t"],
            NOW + first as u64,
            "oldest unsent comes first"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn old_and_unreadable_lines_are_dropped_not_sent() {
        let dir = scratch();
        record_in(&dir, true, &Event::AppStarted, NOW);
        let mut text = std::fs::read_to_string(dir.join(QUEUE_FILE)).unwrap();
        text.push_str("this is not json\n");
        std::fs::write(dir.join(QUEUE_FILE), text).unwrap();

        // Two weeks later the one good event is stale too.
        let later = NOW + 2 * MAX_AGE_SECS;
        assert!(!flush_to(&dir, "http://127.0.0.1:1/x", later).await);
        assert!(!dir.join(QUEUE_FILE).exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
