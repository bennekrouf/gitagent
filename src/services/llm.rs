//! Model providers: a local ollama, or a remote OpenAI-compatible endpoint.
//!
//! Both are reached through one function, `complete_json`, because every model
//! node in the graph has the same contract: a system prompt, a user prompt, and
//! a JSON object back. Free text would force the next node to guess what
//! happened; a parsed object is something the graph can route on.
//!
//! The two wire formats differ enough to be worth noting:
//!
//!   * **ollama** takes a JSON *schema* in `format`, and needs `num_ctx` set
//!     explicitly — it defaults to 4096 regardless of what the model supports,
//!     which silently truncates a diff of any size into confident nonsense.
//!   * **remote** providers are OpenAI-compatible: `POST /chat/completions`
//!     with `response_format: {"type": "json_object"}`. That path covers api0,
//!     OpenAI, Mistral, vLLM, LM Studio and OpenRouter — only the base URL and
//!     model name change.
//!
//! Each provider's API key is read from its environment variable and is never
//! written to disk. api0 also takes the licence number (free or a plan's)
//! already saved on this computer, so it needs no key of its own.

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};
use tokio::sync::Notify;
mod call;
pub use call::*;
mod config;
pub use config::*;

/// Ollama serializes generations on one model by default (`OLLAMA_NUM_PARALLEL=1`),
/// so firing several draft steps at once doesn't run them in parallel — it just
/// queues them on the server, each still burning its own client-side timeout while
/// it waits its turn. Gate calls here instead: only one request is in flight at a
/// time, and the ones behind it wait their turn rather than a socket, and say
/// whose turn it is.
///
/// The gate is shared by every window, and a turn is only given back when the
/// step holding it finishes or is dropped. A step that is neither — parked in a
/// task nothing polls any more — used to keep it for good, and every model step
/// after it sat at "running" with an empty log until GitAgent was restarted. So
/// a holder has to keep checking in while it waits for its answer, and one that
/// stops is passed over: whatever it is doing, it is no longer waiting on ollama.
struct Gate {
    holder: Mutex<Option<Holder>>,
    freed: Notify,
}

struct Holder {
    id: u64,
    what: String,
    since: Instant,
    beat: Instant,
}

fn gate() -> &'static Gate {
    static GATE: OnceLock<Gate> = OnceLock::new();
    GATE.get_or_init(|| Gate {
        holder: Mutex::new(None),
        freed: Notify::new(),
    })
}

/// How often a step waiting on the model checks in, and says how long it has
/// been. Also how often a step waiting for its turn looks again.
const HEARTBEAT: Duration = Duration::from_secs(10);

/// A holder silent for this long is not waiting on an answer any more: the
/// request it sent checks in every `HEARTBEAT`.
const ABANDONED_AFTER: Duration = Duration::from_secs(60);

/// How often a long wait repeats itself in the step's log.
const REMIND_EVERY: Duration = Duration::from_secs(30);

/// One step's turn at the local model. Given back when dropped, which is also
/// what happens to a step that is skipped or cancelled mid-request.
struct Turn {
    id: u64,
}

impl Turn {
    fn beat(&self) {
        let mut holder = gate().holder.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(h) = holder.as_mut().filter(|h| h.id == self.id) {
            h.beat = Instant::now();
        }
    }
}

impl Drop for Turn {
    fn drop(&mut self) {
        let mut holder = gate().holder.lock().unwrap_or_else(|e| e.into_inner());
        // Not ours any more if it was taken over: leave the new holder alone.
        if holder.as_ref().is_some_and(|h| h.id == self.id) {
            *holder = None;
        }
        drop(holder);
        gate().freed.notify_waiters();
    }
}

async fn take_turn(what: &str, on_line: &mut dyn FnMut(&str)) -> Turn {
    take_turn_unless_abandoned(what, on_line, ABANDONED_AFTER).await
}

async fn take_turn_unless_abandoned(
    what: &str,
    on_line: &mut dyn FnMut(&str),
    abandoned_after: Duration,
) -> Turn {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    let id = NEXT.fetch_add(1, Ordering::Relaxed);
    // Whose turn was last reported, and when, so the log says it once and
    // then only every so often — not once a heartbeat.
    let mut reported: Option<(u64, Instant)> = None;

    loop {
        // Registered before the holder is read, so a turn given back between
        // the read and the wait below still wakes this one.
        let freed = gate().freed.notified();
        tokio::pin!(freed);
        freed.as_mut().enable();

        let note = {
            let mut holder = gate().holder.lock().unwrap_or_else(|e| e.into_inner());
            let now = Instant::now();
            let mut taking_over = None;
            match holder.as_ref() {
                Some(h) if h.beat.elapsed() < abandoned_after => {
                    let due = match reported {
                        Some((other, at)) => other != h.id || at.elapsed() >= REMIND_EVERY,
                        None => true,
                    };
                    if due {
                        reported = Some((h.id, now));
                        Some(format!(
                            "Waiting for the model: {} has had it for {}.",
                            h.what,
                            took(h.since.elapsed())
                        ))
                    } else {
                        None
                    }
                }
                gone => {
                    if let Some(h) = gone {
                        taking_over = Some(format!(
                            "{} took the model {} ago and has not been heard from \
                             since, so it is no longer waiting for an answer. Going \
                             ahead without it.",
                            h.what,
                            took(h.since.elapsed())
                        ));
                    }
                    *holder = Some(Holder {
                        id,
                        what: what.to_string(),
                        since: now,
                        beat: now,
                    });
                    drop(holder);
                    if let Some(line) = taking_over {
                        on_line(&line);
                    }
                    return Turn { id };
                }
            }
        };
        if let Some(line) = note {
            on_line(&line);
        }
        let _ = tokio::time::timeout(HEARTBEAT, freed).await;
    }
}

/// "45s", "2m 05s".
fn took(d: Duration) -> String {
    let secs = d.as_secs();
    if secs < 60 {
        format!("{secs}s")
    } else {
        format!("{}m {:02}s", secs / 60, secs % 60)
    }
}

/// Waits for `request`, checking in with the gate and saying in the step's
/// log how long it has been, so a slow answer never looks like a hung step.
async fn awaiting_answer<T>(
    request: impl std::future::Future<Output = T>,
    turn: Option<&Turn>,
    model: &str,
    on_line: &mut dyn FnMut(&str),
) -> T {
    tokio::pin!(request);
    let sent = Instant::now();
    let mut reminded = Instant::now();
    loop {
        tokio::select! {
            biased;
            done = &mut request => return done,
            _ = tokio::time::sleep(HEARTBEAT) => {
                if let Some(turn) = turn {
                    turn.beat();
                }
                if reminded.elapsed() >= REMIND_EVERY {
                    reminded = Instant::now();
                    on_line(&format!(
                        "Still waiting for {model}: {} so far.",
                        took(sent.elapsed())
                    ));
                }
            }
        }
    }
}

/// Which step is asking the model, and where to tell it how the wait is
/// going. Both end up in that step's log while it runs.
pub struct Asker<'a> {
    /// Names the step to any other step waiting for the model behind it.
    pub what: String,
    pub on_line: &'a mut dyn FnMut(&str),
}

/// A remote provider that speaks the OpenAI wire format.
///
/// Every one of these takes `POST {base}/chat/completions` with a bearer token
/// and honours `response_format: {"type": "json_object"}` — including Cohere,
/// through its compatibility endpoint. So they share one client rather than
/// one each, and adding another is a row in this table.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Remote {
    pub key: &'static str,
    pub label: &'static str,
    pub base_url: &'static str,
    pub model: &'static str,
    /// Where the API key is read from. Never stored on disk.
    pub env: &'static str,
}

pub const REMOTES: &[Remote] = &[
    // api0 serves the model the licence's plan pays for; "default" lets it
    // pick. The key is the saved licence, or a licence number in API0_API_KEY.
    Remote {
        key: "api0",
        label: "api0",
        base_url: "https://gateway.api0.ai/v1",
        model: "default",
        env: "API0_API_KEY",
    },
    Remote {
        key: "openai",
        label: "OpenAI",
        base_url: "https://api.openai.com/v1",
        model: "gpt-4o-mini",
        env: "OPENAI_API_KEY",
    },
    Remote {
        key: "mistral",
        label: "Mistral",
        base_url: "https://api.mistral.ai/v1",
        model: "mistral-large-latest",
        env: "MISTRAL_API_KEY",
    },
    Remote {
        key: "cohere",
        label: "Cohere",
        // Cohere's native API is its own shape; this is the compatibility one.
        base_url: "https://api.cohere.ai/compatibility/v1",
        model: "command-r-plus",
        env: "COHERE_API_KEY",
    },
    Remote {
        key: "groq",
        label: "Groq",
        base_url: "https://api.groq.com/openai/v1",
        model: "llama-3.3-70b-versatile",
        env: "GROQ_API_KEY",
    },
    Remote {
        key: "openrouter",
        label: "OpenRouter",
        base_url: "https://openrouter.ai/api/v1",
        model: "openai/gpt-4o-mini",
        env: "OPENROUTER_API_KEY",
    },
];

/// The provider a settings file without one gets, and the one an unknown
/// key falls back to.
const DEFAULT_REMOTE: &str = "openai";

pub fn remote(key: &str) -> &'static Remote {
    REMOTES
        .iter()
        .find(|r| r.key == key)
        .or_else(|| REMOTES.iter().find(|r| r.key == DEFAULT_REMOTE))
        .unwrap_or(&REMOTES[0])
}

/// Whether this provider is api0, which also accepts the saved licence.
pub fn is_api0(remote: &Remote) -> bool {
    remote.key == "api0"
}

/// The key to send to `remote`: its environment variable, or for api0 the
/// licence number saved on this computer.
pub fn key_for(remote: &Remote) -> Option<String> {
    api_key(remote.env).or_else(|| {
        if is_api0(remote) {
            super::licence::saved_key()
        } else {
            None
        }
    })
}

/// One of the AI regions api0 offers. Picking it sends its id as the model;
/// api0 decides what runs there, and its answers only ever name the region.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Region {
    pub id: &'static str,
    pub flag: &'static str,
    pub label: &'static str,
    /// Flag and name, for the tab note and the step log.
    pub display: &'static str,
}

impl Region {
    /// What text shows for this region: flag and name, or the name alone on
    /// Windows, which draws flag emoji as two bare letters.
    pub fn shown(&self) -> &'static str {
        if cfg!(windows) {
            self.label
        } else {
            self.display
        }
    }
}

/// In the order Settings shows them.
// One line per region on purpose: this reads as a table, and rustfmt would
// spread each entry over five lines.
#[rustfmt::skip]
pub const REGIONS: &[Region] = &[
    Region { id: "europe", flag: "🇪🇺", label: "Europe", display: "🇪🇺 Europe" },
    Region { id: "china", flag: "🇨🇳", label: "China", display: "🇨🇳 China" },
    Region { id: "usa", flag: "🇺🇸", label: "United States", display: "🇺🇸 United States" },
];

/// Tokens in a typical model step — a commit message, a pull request
/// description, one review — as measured on api0. Only for the estimate.
pub const TYPICAL_STEP_TOKENS: f64 = 7000.0;

/// What api0 says one of its regions costs right now.
#[derive(Clone, PartialEq, Debug)]
pub struct RegionOffer {
    pub id: String,
    /// Credits on this region cost this many times the plain rate.
    pub multiplier: f64,
    pub tokens_per_credit: i64,
}

impl RegionOffer {
    /// What a typical step costs, as api0 charges it: rounded up, at least 1.
    pub fn credits_per_step(&self) -> i64 {
        let per = self.tokens_per_credit.max(1) as f64;
        ((TYPICAL_STEP_TOKENS * self.multiplier.max(0.0) / per).ceil() as i64).max(1)
    }
}

/// The regions in a `/v1/models` reply from api0.
fn parse_region_offers(v: &Value) -> Vec<RegionOffer> {
    let tokens_per_credit = v["tokens_per_credit"].as_i64().unwrap_or(10_000);
    v["data"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|m| {
            let id = m["id"].as_str()?;
            REGIONS.iter().any(|r| r.id == id).then(|| RegionOffer {
                id: id.to_string(),
                multiplier: m["multiplier"].as_f64().unwrap_or(1.0),
                tokens_per_credit,
            })
        })
        .collect()
}

/// The regions api0 offers this licence now, with what each costs.
pub async fn api0_regions() -> Result<Vec<RegionOffer>, String> {
    let api0 = remote("api0");
    let key = key_for(api0).ok_or_else(|| missing_key(api0))?;
    let url = format!("{}/models", api0.base_url);
    let resp = client(PROBE_TIMEOUT)?
        .get(&url)
        .bearer_auth(key)
        .headers(client_app(api0))
        .send()
        .await
        .map_err(|e| format!("api0 is unreachable: {e}"))?;
    let status = resp.status();
    let text = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        return Err(format!("api0 returned {status}: {}", error_message(&text)));
    }
    let v: Value = serde_json::from_str(&text).map_err(|e| format!("bad reply: {e}"))?;
    Ok(parse_region_offers(&v))
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum ProviderKind {
    Ollama,
    /// Any OpenAI-compatible endpoint. The alias is this kind's old name,
    /// kept so an existing settings.json still loads.
    #[serde(alias = "DeepSeek")]
    Remote,
    /// No model at all. Every step that would have called one is skipped, and
    /// the flows still run — see `flow::without_model` for what stands in.
    ///
    /// A first-class choice, not an error state: plenty of people want the
    /// graph, the approvals and the git handling without a model anywhere near
    /// their code.
    Off,
}

impl ProviderKind {
    pub fn label(self) -> &'static str {
        match self {
            ProviderKind::Ollama => "ollama (local)",
            ProviderKind::Remote => "remote API",
            ProviderKind::Off => "no AI",
        }
    }
}

/// One switch per focused review, by the lens's key.
#[derive(Clone, PartialEq, Debug, Default, Serialize, Deserialize)]
pub struct Lenses {
    #[serde(default)]
    pub alignment: bool,
    #[serde(default)]
    pub security: bool,
    #[serde(default)]
    pub architecture: bool,
}

/// Which model gives the second opinion. Only what differs from the main
/// model is kept: the ollama address and context window, and a proxy URL set
/// for the same remote provider, are shared with it.
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub struct SecondModel {
    pub kind: ProviderKind,
    #[serde(default)]
    pub ollama_model: String,
    #[serde(default = "default_remote")]
    pub remote: String,
    #[serde(default)]
    pub remote_model: String,
}

impl Default for SecondModel {
    fn default() -> Self {
        Self {
            kind: ProviderKind::Off,
            ollama_model: String::new(),
            remote: default_remote(),
            remote_model: String::new(),
        }
    }
}

fn default_remote() -> String {
    DEFAULT_REMOTE.to_string()
}

pub fn api_key(env: &str) -> Option<String> {
    std::env::var(env).ok().filter(|k| !k.is_empty())
}

/// The longest answer any step asks for. Every step replies with a small JSON
/// object; a model that runs past this is rambling, and on a local model each
/// extra token is time.
const ANSWER_TOKENS: u32 = 3072;

/// Instructions and the JSON schema appended to them, in tokens, give or take.
const INSTRUCTION_TOKENS: u32 = 2000;

/// Code is dense: about three characters to a token, sometimes fewer. Erring
/// low means a prompt that fits with room to spare, not one that overflows.
const CHARS_PER_TOKEN: u32 = 3;

/// How long a local model is given to answer.
///
/// Generous on purpose. A 14B model on a laptop spends real minutes on a large
/// prompt — measured here: ~80s for 8k tokens, past five minutes for 14k — and
/// the old 300s cut those off mid-generation. A wait you can see the end of
/// beats a failure you have to diagnose.
const OLLAMA_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(900);

/// A hosted endpoint that has not answered in this long is not going to.
const REMOTE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(300);

/// "Can I reach it at all", which is a question with a fast answer. The
/// settings panel blocks on this, so it must never inherit a generation-sized
/// wait.
const PROBE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);

fn client(timeout: std::time::Duration) -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .timeout(timeout)
        .build()
        .map_err(|e| format!("http client: {e}"))
}

/// A reply stopped at `ANSWER_TOKENS` is half an object, and the raw text
/// that a parse error would show is mostly the loop that ran it out — the
/// same token over and over. Say what happened instead.
fn cut_off(model: &str) -> String {
    format!(
        "{model} ran out of room before it finished its answer ({ANSWER_TOKENS} tokens), \
         so the reply was cut off and cannot be read. Small models do this when they \
         start repeating themselves. Run the step again, or pick a larger model in Settings."
    )
}

/// A model ollama has on disk.
#[derive(Clone, PartialEq, Debug)]
pub struct LocalModel {
    pub name: String,
    /// Bytes on disk.
    pub size: u64,
}

impl LocalModel {
    /// "9.0 GB", "1.6 GB" — ollama's own way of saying it.
    pub fn size_label(&self) -> String {
        format!("{:.1} GB", self.size as f64 / 1e9)
    }
}

/// Every model the ollama at `url` has pulled, so Settings can offer them
/// instead of asking for a name to be typed exactly right.
pub async fn local_models(url: &str) -> Result<Vec<LocalModel>, String> {
    let url = format!("{}/api/tags", url.trim_end_matches('/'));
    let resp = client(PROBE_TIMEOUT)?
        .get(&url)
        .send()
        .await
        .map_err(|e| format!("ollama is not answering at {url} ({e})"))?;
    let value: Value = resp.json().await.map_err(|e| format!("bad reply: {e}"))?;
    Ok(parse_tags(&value))
}

/// api0 is told which app is calling, so its use is put down to GitAgent.
fn client_app(remote: &Remote) -> reqwest::header::HeaderMap {
    let mut headers = reqwest::header::HeaderMap::new();
    if is_api0(remote) {
        headers.insert(
            "X-Client-App",
            reqwest::header::HeaderValue::from_static(super::licence::PRODUCT),
        );
    }
    headers
}

/// What to do about a provider with no key.
fn missing_key(remote: &Remote) -> String {
    if is_api0(remote) {
        format!(
            "api0 needs a licence — get free AI credits by email under Get Pro\u{2026}, \
             or a plan for more ({}), or export a licence number as {} and restart the app",
            super::licence::PRICING_URL,
            remote.env
        )
    } else {
        format!("{} is not set — export it and restart the app", remote.env)
    }
}

/// The message out of an OpenAI-style error body, or the body itself. A 402
/// from api0 says what to do about the credits; the raw JSON around it doesn't.
fn error_message(body: &str) -> String {
    serde_json::from_str::<Value>(body)
        .ok()
        .and_then(|v| v["error"]["message"].as_str().map(str::to_string))
        .unwrap_or_else(|| body.to_string())
}

fn api0_balance_line(v: &Value) -> Result<String, String> {
    let credits = v["balance"].as_i64().unwrap_or(0);
    if v["plan"].as_bool() != Some(true) {
        return Err("api0 accepted the licence, but it has no plan with AI credits".into());
    }
    if credits <= 0 {
        return Err("api0: this month's AI credits are used up".into());
    }
    Ok(format!(
        "api0 authenticated — {credits} AI credits left this month"
    ))
}

#[cfg(test)]
mod tests;
