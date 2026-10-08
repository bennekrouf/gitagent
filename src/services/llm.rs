//! Model providers: a local ollama, or a remote DeepSeek endpoint.
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
//!   * **DeepSeek** is OpenAI-compatible: `POST /chat/completions` with
//!     `response_format: {"type": "json_object"}`. That path also covers
//!     OpenAI, vLLM, LM Studio and OpenRouter later — only the base URL and
//!     model name change.
//!
//! The API key is read from `DEEPSEEK_API_KEY` and is never written to disk.

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};
use tokio::sync::Notify;

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
    Remote {
        key: "deepseek",
        label: "DeepSeek",
        base_url: "https://api.deepseek.com/v1",
        model: "deepseek-chat",
        env: "DEEPSEEK_API_KEY",
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
        model: "anthropic/claude-3.5-sonnet",
        env: "OPENROUTER_API_KEY",
    },
];

pub fn remote(key: &str) -> &'static Remote {
    REMOTES.iter().find(|r| r.key == key).unwrap_or(&REMOTES[0])
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum ProviderKind {
    Ollama,
    /// Any OpenAI-compatible endpoint. `DeepSeek` is the old name for this,
    /// kept as an alias so an existing settings.json still loads.
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

#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub struct LlmConfig {
    pub kind: ProviderKind,
    pub ollama_url: String,
    pub ollama_model: String,
    /// Explicit, because ollama's default of 4096 is smaller than any real diff.
    pub ollama_num_ctx: u32,
    /// Which entry of `REMOTES` is selected, by `Remote::key`.
    #[serde(default = "default_remote")]
    pub remote: String,
    /// Overrides the preset's base URL when non-empty — for a proxy, a
    /// self-hosted vLLM, or a provider not in the list.
    #[serde(default, alias = "deepseek_url")]
    pub remote_url: String,
    /// Overrides the preset's model when non-empty.
    #[serde(default, alias = "deepseek_model")]
    pub remote_model: String,
    /// A second model that reviews every pull request alongside the first.
    /// Off unless chosen: it is one more model call per review.
    #[serde(default)]
    pub second: SecondModel,
    /// Which focused reviews run beside the regression review. All off unless
    /// turned on: each is one more model call per review.
    #[serde(default)]
    pub lenses: Lenses,
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

impl Lenses {
    pub fn is_on(&self, key: &str) -> bool {
        match key {
            "alignment" => self.alignment,
            "security" => self.security,
            "architecture" => self.architecture,
            _ => false,
        }
    }

    pub fn set(&mut self, key: &str, on: bool) {
        match key {
            "alignment" => self.alignment = on,
            "security" => self.security = on,
            "architecture" => self.architecture = on,
            _ => {}
        }
    }

    pub fn any(&self) -> bool {
        self.alignment || self.security || self.architecture
    }
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
    REMOTES[0].key.to_string()
}

impl Default for LlmConfig {
    fn default() -> Self {
        Self {
            kind: ProviderKind::Ollama,
            ollama_url: "http://localhost:11434".into(),
            ollama_model: "qwen2.5-coder:14b".into(),
            ollama_num_ctx: 16384,
            remote: default_remote(),
            remote_url: String::new(),
            remote_model: String::new(),
            second: SecondModel::default(),
            lenses: Lenses::default(),
        }
    }
}

impl LlmConfig {
    pub fn active_model(&self) -> &str {
        match self.kind {
            ProviderKind::Ollama => &self.ollama_model,
            ProviderKind::Remote => self.remote_model_name(),
            ProviderKind::Off => "no AI",
        }
    }
}

impl LlmConfig {
    /// The second reviewer as a full config, ready to call. `Off` when none
    /// is chosen — or when this install runs without AI at all, which a second
    /// opinion must not quietly overrule.
    pub fn second_config(&self) -> LlmConfig {
        let s = &self.second;
        LlmConfig {
            kind: if self.uses_model() {
                s.kind
            } else {
                ProviderKind::Off
            },
            ollama_url: self.ollama_url.clone(),
            ollama_model: if s.ollama_model.trim().is_empty() {
                self.ollama_model.clone()
            } else {
                s.ollama_model.trim().to_string()
            },
            ollama_num_ctx: self.ollama_num_ctx,
            remote: s.remote.clone(),
            remote_url: if s.remote == self.remote {
                self.remote_url.clone()
            } else {
                String::new()
            },
            remote_model: s.remote_model.clone(),
            second: SecondModel::default(),
            lenses: Lenses::default(),
        }
    }

    pub fn preset(&self) -> &'static Remote {
        remote(&self.remote)
    }

    /// The preset's value unless overridden — so switching provider needs one
    /// click, and a proxy or self-hosted endpoint is still one field away.
    pub fn remote_base_url(&self) -> &str {
        if self.remote_url.trim().is_empty() {
            self.preset().base_url
        } else {
            self.remote_url.trim()
        }
    }

    /// Whether it is safe to put the API key on a request to this endpoint.
    ///
    /// The base URL is free text in Settings, and every remote call attaches
    /// a bearer token to it. Over plain `http` that token crosses the network
    /// in the clear, so a mistyped scheme leaks the key to anything on the
    /// path. Loopback is exempt: a local vLLM or LM Studio on `http://
    /// localhost` is a normal setup and never leaves the machine.
    pub fn endpoint_carries_key_safely(&self) -> bool {
        let url = self.remote_base_url();
        if url.starts_with("https://") {
            return true;
        }
        let Some(rest) = url.strip_prefix("http://") else {
            return false;
        };
        let authority = rest.split('/').next().unwrap_or("");
        // An IPv6 literal is bracketed and full of colons, so the port
        // cannot simply be split off at the first one.
        let host = match authority.strip_prefix('[') {
            Some(v6) => v6.split(']').next().unwrap_or(""),
            None => authority.split(':').next().unwrap_or(""),
        };
        matches!(host, "localhost" | "127.0.0.1" | "::1")
    }

    pub fn remote_model_name(&self) -> &str {
        if self.remote_model.trim().is_empty() {
            self.preset().model
        } else {
            self.remote_model.trim()
        }
    }

    /// The API key for the selected provider, from its environment variable.
    /// Whether any step is allowed to call a model.
    pub fn uses_model(&self) -> bool {
        self.kind != ProviderKind::Off
    }

    pub fn remote_key(&self) -> Option<String> {
        api_key(self.preset().env)
    }
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

impl LlmConfig {
    /// How many characters of input a step may send this model, diff and all.
    ///
    /// Local: whatever the context window leaves after the instructions and
    /// the answer. Past it, ollama silently drops the *start* of the prompt —
    /// the instructions — and spends the longest possible time doing so.
    /// Lowering the context window in Settings is the way to trade coverage
    /// for speed.
    ///
    /// Remote: every preset's window is far larger than a diff worth
    /// reviewing, so the ceiling is cost, not fit — the same cap a diff is
    /// stored under.
    pub fn input_budget(&self) -> usize {
        match self.kind {
            ProviderKind::Ollama => {
                let tokens = self
                    .ollama_num_ctx
                    .saturating_sub(INSTRUCTION_TOKENS + ANSWER_TOKENS)
                    .max(1000);
                ((tokens * CHARS_PER_TOKEN) as usize).min(super::git::DIFF_CAP)
            }
            ProviderKind::Remote | ProviderKind::Off => super::git::DIFF_CAP,
        }
    }
}

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

/// Runs one model call and parses the reply as a JSON object.
///
/// `schema` is a JSON Schema describing the expected object. ollama enforces it
/// server-side; DeepSeek only guarantees *valid* JSON, so the schema is also
/// rendered into the system prompt for both providers. Structure you asked for
/// in the prompt and structure you validated on the way out are different
/// things — the caller still checks the fields it needs.
pub async fn complete_json(
    cfg: &LlmConfig,
    system: &str,
    user: &str,
    schema: &Value,
    asker: &mut Asker<'_>,
) -> Result<Value, String> {
    let system = format!(
        "{system}\n\nReply with a single JSON object matching this schema, and \
         nothing else — no prose, no markdown fence:\n{}",
        serde_json::to_string_pretty(schema).unwrap_or_default()
    );

    let raw = match cfg.kind {
        ProviderKind::Ollama => call_ollama(cfg, &system, user, schema, asker).await?,
        ProviderKind::Remote => call_openai_compatible(cfg, &system, user, asker).await?,
        // Reaching here means a model step ran with no model configured, which
        // the executor is supposed to have headed off. Say which of the two is
        // broken rather than pretending the provider is unreachable.
        ProviderKind::Off => {
            return Err(
                "this step needs a model, but this install is set to run without one. \
                 Pick a provider in Settings, or skip the step."
                    .into(),
            )
        }
    };

    parse_object(&raw)
}

/// Models wrap JSON in ```json fences often enough to be worth handling here
/// rather than in every caller.
fn parse_object(raw: &str) -> Result<Value, String> {
    let trimmed = raw.trim();
    let body = trimmed
        .strip_prefix("```json")
        .or_else(|| trimmed.strip_prefix("```"))
        .map(|s| s.trim_start().trim_end_matches("```").trim())
        .unwrap_or(trimmed);

    let value: Value = serde_json::from_str(body)
        .map_err(|e| format!("model did not return JSON ({e}). Raw reply:\n{raw}"))?;

    if !value.is_object() {
        return Err(format!("expected a JSON object, got: {raw}"));
    }
    Ok(value)
}

async fn call_ollama(
    cfg: &LlmConfig,
    system: &str,
    user: &str,
    schema: &Value,
    asker: &mut Asker<'_>,
) -> Result<String, String> {
    let url = format!("{}/api/chat", cfg.ollama_url.trim_end_matches('/'));
    let body = json!({
        "model": cfg.ollama_model,
        "stream": false,
        "format": schema,
        "options": {
            // Deterministic on purpose: the same diff should produce the same
            // commit message twice, or the approval step is meaningless.
            "temperature": 0,
            "seed": 7,
            "num_ctx": cfg.ollama_num_ctx,
            "num_predict": ANSWER_TOKENS,
        },
        "messages": [
            { "role": "system", "content": system },
            { "role": "user",   "content": user },
        ],
    });

    // Wait for our turn at the (serialized) local model before spending any
    // of the generation timeout, so queued steps wait here rather than racing
    // a clock they can't win.
    let turn = take_turn(&asker.what, asker.on_line).await;
    (asker.on_line)(&format!(
        "Sent to {} ({} KB). Waiting for its answer…",
        cfg.ollama_model,
        (system.len() + user.len()).div_ceil(1024)
    ));

    let request = async {
        let resp = client(OLLAMA_TIMEOUT)?
            .post(&url)
            .json(&body)
            .send()
            .await
            .map_err(|e| {
                // A timeout is not a missing server, and saying so sent people
                // to check `ollama serve` — which was running the whole time —
                // while the actual problem was a prompt this model cannot
                // answer inside the wait. Name which of the two happened, and
                // what to do.
                if e.is_timeout() {
                    format!(
                        "{} did not answer within {}s. That is the model being too slow for \
                         this prompt, not ollama being down. Skip this step, lower the context \
                         window in Settings so it is sent less (it gets {} characters now), or \
                         use a smaller model.",
                        cfg.ollama_model,
                        OLLAMA_TIMEOUT.as_secs(),
                        cfg.input_budget(),
                    )
                } else {
                    format!("ollama unreachable at {url} — is `ollama serve` running? ({e})")
                }
            })?;
        let status = resp.status();
        Ok::<_, String>((status, resp.text().await.unwrap_or_default()))
    };
    let (status, text) =
        awaiting_answer(request, Some(&turn), &cfg.ollama_model, asker.on_line).await?;
    drop(turn);

    if !status.is_success() {
        return Err(format!("ollama returned {status}: {text}"));
    }

    let value: Value =
        serde_json::from_str(&text).map_err(|e| format!("ollama sent invalid JSON: {e}"))?;
    value["message"]["content"]
        .as_str()
        .map(|s| s.to_string())
        .ok_or_else(|| format!("ollama reply had no message.content: {text}"))
}

/// One client for every OpenAI-compatible provider. Only the base URL, the
/// model and the key's environment variable differ between them.
async fn call_openai_compatible(
    cfg: &LlmConfig,
    system: &str,
    user: &str,
    asker: &mut Asker<'_>,
) -> Result<String, String> {
    let preset = cfg.preset();
    if !cfg.endpoint_carries_key_safely() {
        return Err(format!(
            "refusing to send {} to {} — it is not https, and a bearer token over plain \
             http crosses the network in the clear. Use https, or a loopback address for \
             a local endpoint.",
            preset.env,
            cfg.remote_base_url()
        ));
    }
    let key = cfg
        .remote_key()
        .ok_or_else(|| format!("{} is not set — export it and restart the app", preset.env))?;
    let url = format!(
        "{}/chat/completions",
        cfg.remote_base_url().trim_end_matches('/')
    );
    let body = json!({
        "model": cfg.remote_model_name(),
        "stream": false,
        "temperature": 0,
        "max_tokens": ANSWER_TOKENS,
        "response_format": { "type": "json_object" },
        "messages": [
            { "role": "system", "content": system },
            { "role": "user",   "content": user },
        ],
    });

    (asker.on_line)(&format!(
        "Sent to {} on {}. Waiting for its answer…",
        cfg.remote_model_name(),
        preset.label
    ));
    let request = async {
        let resp = client(REMOTE_TIMEOUT)?
            .post(&url)
            .bearer_auth(key)
            .json(&body)
            .send()
            .await
            .map_err(|e| format!("{} unreachable at {url}: {e}", preset.label))?;
        let status = resp.status();
        Ok::<_, String>((status, resp.text().await.unwrap_or_default()))
    };
    let (status, text) =
        awaiting_answer(request, None, cfg.remote_model_name(), asker.on_line).await?;
    if !status.is_success() {
        return Err(format!("{} returned {status}: {text}", preset.label));
    }

    let value: Value = serde_json::from_str(&text)
        .map_err(|e| format!("{} sent invalid JSON: {e}", preset.label))?;
    value["choices"][0]["message"]["content"]
        .as_str()
        .map(|s| s.to_string())
        .ok_or_else(|| {
            format!(
                "{} reply had no choices[0].message.content: {text}",
                preset.label
            )
        })
}

/// Cheap reachability check for the settings panel.
pub async fn probe(cfg: &LlmConfig) -> Result<String, String> {
    match cfg.kind {
        ProviderKind::Off => Ok("no AI — model steps are skipped".into()),
        ProviderKind::Ollama => {
            let url = format!("{}/api/tags", cfg.ollama_url.trim_end_matches('/'));
            let resp = client(PROBE_TIMEOUT)?
                .get(&url)
                .send()
                .await
                .map_err(|e| format!("unreachable: {e}"))?;
            let value: Value = resp.json().await.map_err(|e| format!("bad reply: {e}"))?;
            let models: Vec<&str> = value["models"]
                .as_array()
                .map(|a| a.iter().filter_map(|m| m["name"].as_str()).collect())
                .unwrap_or_default();
            if models.iter().any(|m| *m == cfg.ollama_model) {
                Ok(format!("{} is loaded and ready", cfg.ollama_model))
            } else {
                Err(format!(
                    "reachable, but {} is not pulled. Available: {}",
                    cfg.ollama_model,
                    if models.is_empty() {
                        "none".into()
                    } else {
                        models.join(", ")
                    }
                ))
            }
        }
        ProviderKind::Remote => {
            let preset = cfg.preset();
            if !cfg.endpoint_carries_key_safely() {
                return Err(format!(
                    "{} is not https — refusing to send {} over it in the clear",
                    cfg.remote_base_url(),
                    preset.env
                ));
            }
            let Some(key) = cfg.remote_key() else {
                return Err(format!("{} is not set", preset.env));
            };
            let url = format!("{}/models", cfg.remote_base_url().trim_end_matches('/'));
            let resp = client(PROBE_TIMEOUT)?
                .get(&url)
                .bearer_auth(key)
                .send()
                .await
                .map_err(|e| format!("unreachable: {e}"))?;
            if resp.status().is_success() {
                Ok(format!(
                    "{} authenticated, using {}",
                    preset.label,
                    cfg.remote_model_name()
                ))
            } else {
                Err(format!("{} — check {}", resp.status(), preset.env))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn there_is_no_second_reviewer_until_one_is_chosen() {
        assert!(!LlmConfig::default().second_config().uses_model());
    }

    #[test]
    fn the_second_reviewer_shares_the_main_models_connection() {
        let mut cfg = LlmConfig {
            kind: ProviderKind::Remote,
            remote: "openai".into(),
            remote_url: "https://proxy.example/v1".into(),
            ..Default::default()
        };
        cfg.second = SecondModel {
            kind: ProviderKind::Remote,
            remote: "openai".into(),
            remote_model: "gpt-5".into(),
            ..Default::default()
        };
        let second = cfg.second_config();
        assert_eq!(second.remote_base_url(), "https://proxy.example/v1");
        assert_eq!(second.active_model(), "gpt-5");
        cfg.second.remote = "deepseek".into();
        assert_eq!(
            cfg.second_config().remote_base_url(),
            "https://api.deepseek.com/v1"
        );
    }

    #[test]
    fn a_second_reviewer_does_not_switch_ai_back_on() {
        let mut cfg = LlmConfig {
            kind: ProviderKind::Off,
            ..Default::default()
        };
        cfg.second.kind = ProviderKind::Ollama;
        assert!(!cfg.second_config().uses_model());
    }

    use super::*;

    #[test]
    fn a_bearer_token_is_not_sent_over_plain_http() {
        let mut cfg = LlmConfig {
            kind: ProviderKind::Remote,
            remote_url: "http://api.example.com/v1".into(),
            ..Default::default()
        };
        assert!(!cfg.endpoint_carries_key_safely());

        cfg.remote_url = "https://api.example.com/v1".into();
        assert!(cfg.endpoint_carries_key_safely());
    }

    #[test]
    fn a_local_endpoint_over_http_is_still_allowed() {
        // vLLM or LM Studio on loopback never leaves the machine.
        for url in [
            "http://localhost:8000/v1",
            "http://127.0.0.1:1234/v1",
            "http://[::1]:8000/v1",
        ] {
            let cfg = LlmConfig {
                kind: ProviderKind::Remote,
                remote_url: url.into(),
                ..Default::default()
            };
            assert!(cfg.endpoint_carries_key_safely(), "{url}");
        }
    }

    #[test]
    fn every_shipped_preset_is_https() {
        for preset in REMOTES {
            let cfg = LlmConfig {
                kind: ProviderKind::Remote,
                remote: preset.key.into(),
                ..Default::default()
            };
            assert!(cfg.endpoint_carries_key_safely(), "{}", preset.key);
        }
    }

    #[tokio::test]
    async fn a_turn_nobody_is_waiting_on_does_not_block_the_model_for_good() {
        // One test rather than several: the gate is process-wide, and tests
        // run in parallel.
        let mut lines: Vec<String> = vec![];

        let first = take_turn("Draft commit message in other", &mut |l| {
            lines.push(l.to_string())
        })
        .await;

        // Still checking in: whoever comes next waits, and says for whom.
        let waited = tokio::time::timeout(
            Duration::from_millis(200),
            take_turn_unless_abandoned(
                "Draft PR description in mayorana",
                &mut |_| {},
                ABANDONED_AFTER,
            ),
        )
        .await;
        assert!(waited.is_err(), "a live holder keeps its turn");

        // Silent past the limit: passed over, and the log says why.
        let second = take_turn_unless_abandoned(
            "Draft commit message in mayorana",
            &mut |l| lines.push(l.to_string()),
            Duration::ZERO,
        )
        .await;
        assert!(lines
            .iter()
            .any(|l| l.contains("Draft commit message in other") && l.contains("Going ahead")));

        // The abandoned holder finishing late must not free the new turn.
        drop(first);
        let blocked = tokio::time::timeout(
            Duration::from_millis(200),
            take_turn_unless_abandoned("Review in api0", &mut |_| {}, ABANDONED_AFTER),
        )
        .await;
        assert!(blocked.is_err(), "the takeover's turn is still held");

        drop(second);
        let third = tokio::time::timeout(
            Duration::from_secs(1),
            take_turn_unless_abandoned("Review in api0", &mut |_| {}, ABANDONED_AFTER),
        )
        .await;
        assert!(third.is_ok(), "a turn given back frees the model");
    }

    #[test]
    fn waits_read_as_minutes_and_seconds() {
        assert_eq!(took(Duration::from_secs(45)), "45s");
        assert_eq!(took(Duration::from_secs(125)), "2m 05s");
    }

    #[test]
    fn bare_json_parses() {
        let v = parse_object(r#"{"subject":"fix: thing"}"#).unwrap();
        assert_eq!(v["subject"], "fix: thing");
    }

    #[test]
    fn a_fenced_reply_parses_too() {
        let v = parse_object("```json\n{\"subject\":\"fix: thing\"}\n```").unwrap();
        assert_eq!(v["subject"], "fix: thing");
    }

    #[test]
    fn prose_is_an_error_not_a_silent_empty_object() {
        assert!(parse_object("Sure! Here is your commit message.").is_err());
    }

    #[test]
    fn a_bare_array_is_rejected() {
        assert!(parse_object("[1, 2, 3]").is_err());
    }

    #[test]
    fn every_provider_is_reachable_by_key_and_none_collide() {
        let mut keys: Vec<&str> = REMOTES.iter().map(|r| r.key).collect();
        let count = keys.len();
        keys.sort_unstable();
        keys.dedup();
        assert_eq!(keys.len(), count, "duplicate provider key");
        for entry in REMOTES {
            assert_eq!(remote(entry.key).label, entry.label);
        }
    }

    #[test]
    fn an_unknown_provider_falls_back_rather_than_panicking() {
        assert_eq!(remote("does-not-exist").key, REMOTES[0].key);
    }

    #[test]
    fn each_provider_reads_its_own_environment_variable() {
        // Switching provider must not keep looking for the previous key.
        for (key, env) in [
            ("openai", "OPENAI_API_KEY"),
            ("mistral", "MISTRAL_API_KEY"),
            ("cohere", "COHERE_API_KEY"),
        ] {
            let cfg = LlmConfig {
                kind: ProviderKind::Remote,
                remote: key.into(),
                ..LlmConfig::default()
            };
            assert_eq!(cfg.preset().env, env);
        }
    }

    #[test]
    fn the_preset_supplies_url_and_model_until_overridden() {
        let mut cfg = LlmConfig {
            kind: ProviderKind::Remote,
            remote: "mistral".into(),
            ..LlmConfig::default()
        };
        assert_eq!(cfg.remote_base_url(), "https://api.mistral.ai/v1");
        assert_eq!(cfg.remote_model_name(), "mistral-large-latest");

        cfg.remote_url = "http://localhost:8000/v1".into();
        cfg.remote_model = "my-own-model".into();
        assert_eq!(cfg.remote_base_url(), "http://localhost:8000/v1");
        assert_eq!(cfg.remote_model_name(), "my-own-model");
    }

    #[test]
    fn whitespace_is_not_an_override() {
        let cfg = LlmConfig {
            kind: ProviderKind::Remote,
            remote_url: "   ".into(),
            ..LlmConfig::default()
        };
        assert_eq!(cfg.remote_base_url(), cfg.preset().base_url);
    }

    #[test]
    fn a_settings_file_written_before_this_change_still_loads() {
        // The old shape named DeepSeek directly and had its own url/model keys.
        let old = r#"{
            "kind": "DeepSeek",
            "ollama_url": "http://localhost:11434",
            "ollama_model": "qwen2.5-coder:14b",
            "ollama_num_ctx": 16384,
            "deepseek_url": "https://api.deepseek.com/v1",
            "deepseek_model": "deepseek-chat"
        }"#;
        let cfg: LlmConfig = serde_json::from_str(old).expect("old settings must still parse");
        assert_eq!(cfg.kind, ProviderKind::Remote);
        assert_eq!(cfg.remote_base_url(), "https://api.deepseek.com/v1");
        assert_eq!(cfg.remote_model_name(), "deepseek-chat");
    }

    #[test]
    fn the_default_context_is_large_enough_for_a_real_diff() {
        // Guards against regressing to ollama's 4096 default.
        assert!(LlmConfig::default().ollama_num_ctx >= 16384);
    }

    #[test]
    fn a_local_model_is_sent_what_its_window_holds() {
        let mut cfg = LlmConfig::default();
        // 16384 - 2000 - 3072 tokens, three characters each.
        assert_eq!(cfg.input_budget(), 33_936);
        cfg.ollama_num_ctx = 8192;
        assert_eq!(
            cfg.input_budget(),
            9_360,
            "a smaller window is a faster answer"
        );
        cfg.ollama_num_ctx = 131_072;
        assert_eq!(cfg.input_budget(), crate::services::git::DIFF_CAP);
        cfg.ollama_num_ctx = 2048;
        assert_eq!(cfg.input_budget(), 3_000, "never nothing at all");
    }

    #[test]
    fn a_remote_model_is_capped_by_cost_not_by_its_window() {
        let cfg = LlmConfig {
            kind: ProviderKind::Remote,
            ..LlmConfig::default()
        };
        assert_eq!(cfg.input_budget(), crate::services::git::DIFF_CAP);
    }
}
