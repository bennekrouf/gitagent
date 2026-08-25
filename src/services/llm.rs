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

pub const DEEPSEEK_KEY_ENV: &str = "DEEPSEEK_API_KEY";

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum ProviderKind {
    Ollama,
    DeepSeek,
}

impl ProviderKind {
    pub fn label(self) -> &'static str {
        match self {
            ProviderKind::Ollama => "ollama (local)",
            ProviderKind::DeepSeek => "DeepSeek (remote)",
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
    pub deepseek_url: String,
    pub deepseek_model: String,
}

impl Default for LlmConfig {
    fn default() -> Self {
        Self {
            kind: ProviderKind::Ollama,
            ollama_url: "http://localhost:11434".into(),
            ollama_model: "qwen2.5-coder:14b".into(),
            ollama_num_ctx: 16384,
            deepseek_url: "https://api.deepseek.com/v1".into(),
            deepseek_model: "deepseek-chat".into(),
        }
    }
}

impl LlmConfig {
    pub fn active_model(&self) -> &str {
        match self.kind {
            ProviderKind::Ollama => &self.ollama_model,
            ProviderKind::DeepSeek => &self.deepseek_model,
        }
    }
}

pub fn deepseek_key() -> Option<String> {
    std::env::var(DEEPSEEK_KEY_ENV)
        .ok()
        .filter(|k| !k.is_empty())
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
        ProviderKind::DeepSeek => call_deepseek(cfg, &system, user).await?,
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

async fn call_deepseek(cfg: &LlmConfig, system: &str, user: &str) -> Result<String, String> {
    let key = deepseek_key()
        .ok_or_else(|| format!("{DEEPSEEK_KEY_ENV} is not set — export it and restart the app"))?;
    let url = format!(
        "{}/chat/completions",
        cfg.deepseek_url.trim_end_matches('/')
    );
    let body = json!({
        "model": cfg.deepseek_model,
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
        ProviderKind::DeepSeek => {
            if deepseek_key().is_none() {
                return Err(format!("{DEEPSEEK_KEY_ENV} is not set"));
            }
            let url = format!("{}/models", cfg.deepseek_url.trim_end_matches('/'));
            let resp = client()?
                .get(&url)
                .bearer_auth(deepseek_key().unwrap_or_default())
                .send()
                .await
                .map_err(|e| format!("unreachable: {e}"))?;
            if resp.status().is_success() {
                Ok(format!("authenticated, using {}", cfg.deepseek_model))
            } else {
                Err(format!("{} — check the API key", resp.status()))
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
    fn the_default_context_is_large_enough_for_a_real_diff() {
        // Guards against regressing to ollama's 4096 default.
        assert!(LlmConfig::default().ollama_num_ctx >= 16384);
    }
}
