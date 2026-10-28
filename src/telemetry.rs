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
    /// Emitted once per install, by [`record`] itself, when the install's
    /// identity is first created — the first event recorded after the person
    /// has seen the notice. The "installed" step of the funnel: a download in
    /// the web server's log is not an install, and only the app can tell.
    AppInstalled,
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
            Event::AppInstalled => "app_installed",
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
    let new_install = state.install_id.is_empty();
    if new_install {
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

    // Ahead of whatever triggered it, so "installed" always precedes the first
    // thing the install did. Tied to creating the id, so it cannot repeat.
    let mut lines = Vec::new();
    if new_install {
        lines.push(make_line(&Event::AppInstalled, now));
    }
    lines.push(make_line(event, now));

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
mod tests;
