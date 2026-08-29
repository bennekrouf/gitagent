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
}

impl ProviderKind {
    pub fn label(self) -> &'static str {
        match self {
            ProviderKind::Ollama => "ollama (local)",
            ProviderKind::Remote => "remote API",
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
        }
    }
}

impl LlmConfig {
    pub fn active_model(&self) -> &str {
        match self.kind {
            ProviderKind::Ollama => &self.ollama_model,
            ProviderKind::Remote => self.remote_model_name(),
        }
    }
}

impl LlmConfig {
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

    pub fn remote_model_name(&self) -> &str {
        if self.remote_model.trim().is_empty() {
            self.preset().model
        } else {
            self.remote_model.trim()
        }
    }

    /// The API key for the selected provider, from its environment variable.
    pub fn remote_key(&self) -> Option<String> {
        api_key(self.preset().env)
    }
}

pub fn api_key(env: &str) -> Option<String> {
    std::env::var(env).ok().filter(|k| !k.is_empty())
}

fn client() -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(300))
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
) -> Result<Value, String> {
    let system = format!(
        "{system}\n\nReply with a single JSON object matching this schema, and \
         nothing else — no prose, no markdown fence:\n{}",
        serde_json::to_string_pretty(schema).unwrap_or_default()
    );

    let raw = match cfg.kind {
        ProviderKind::Ollama => call_ollama(cfg, &system, user, schema).await?,
        ProviderKind::Remote => call_openai_compatible(cfg, &system, user).await?,
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
        },
        "messages": [
            { "role": "system", "content": system },
            { "role": "user",   "content": user },
        ],
    });

    let resp =
        client()?.post(&url).json(&body).send().await.map_err(|e| {
            format!("ollama unreachable at {url} — is `ollama serve` running? ({e})")
        })?;

    let status = resp.status();
    let text = resp.text().await.unwrap_or_default();
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
) -> Result<String, String> {
    let preset = cfg.preset();
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
        "response_format": { "type": "json_object" },
        "messages": [
            { "role": "system", "content": system },
            { "role": "user",   "content": user },
        ],
    });

    let resp = client()?
        .post(&url)
        .bearer_auth(key)
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("DeepSeek unreachable at {url}: {e}"))?;

    let status = resp.status();
    let text = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        return Err(format!("DeepSeek returned {status}: {text}"));
    }

    let value: Value =
        serde_json::from_str(&text).map_err(|e| format!("DeepSeek sent invalid JSON: {e}"))?;
    value["choices"][0]["message"]["content"]
        .as_str()
        .map(|s| s.to_string())
        .ok_or_else(|| format!("DeepSeek reply had no choices[0].message.content: {text}"))
}

/// Cheap reachability check for the settings panel.
pub async fn probe(cfg: &LlmConfig) -> Result<String, String> {
    match cfg.kind {
        ProviderKind::Ollama => {
            let url = format!("{}/api/tags", cfg.ollama_url.trim_end_matches('/'));
            let resp = client()?
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
            let Some(key) = cfg.remote_key() else {
                return Err(format!("{} is not set", preset.env));
            };
            let url = format!("{}/models", cfg.remote_base_url().trim_end_matches('/'));
            let resp = client()?
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
    use super::*;

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
}
