//! Calls to a model: one JSON completion, per provider, and probing what a
//! provider offers.

use super::*;

/// Runs one model call and parses the reply as a JSON object.
///
/// `schema` is a JSON Schema describing the expected object. ollama enforces it
/// server-side; an OpenAI-compatible API only guarantees *valid* JSON, so the schema is also
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
pub(super) fn parse_object(raw: &str) -> Result<Value, String> {
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

pub(super) async fn call_ollama(
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
        // Every call here wants the answer, never the reasoning. A model that
        // thinks by default — MiniCPM5 does — spent the whole answer budget
        // thinking about a 5 KB diff and returned nothing at all. Models that
        // cannot think accept the flag and ignore it.
        "think": false,
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
    if value["done_reason"] == "length" {
        return Err(cut_off(&cfg.ollama_model));
    }
    value["message"]["content"]
        .as_str()
        .map(|s| s.to_string())
        .ok_or_else(|| format!("ollama reply had no message.content: {text}"))
}

/// One client for every OpenAI-compatible provider. Only the base URL, the
/// model and the key's environment variable differ between them.
pub(super) async fn call_openai_compatible(
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
    let key = cfg.remote_key().ok_or_else(|| missing_key(preset))?;
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
            .headers(client_app(preset))
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
        return Err(format!(
            "{} returned {status}: {}",
            preset.label,
            error_message(&text)
        ));
    }

    let value: Value = serde_json::from_str(&text)
        .map_err(|e| format!("{} sent invalid JSON: {e}", preset.label))?;
    if value["choices"][0]["finish_reason"] == "length" {
        return Err(cut_off(cfg.remote_model_name()));
    }
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

/// The models in an `/api/tags` reply, by name.
pub(super) fn parse_tags(reply: &Value) -> Vec<LocalModel> {
    let mut models: Vec<LocalModel> = reply["models"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|m| {
                    Some(LocalModel {
                        name: m["name"].as_str()?.to_string(),
                        size: m["size"].as_u64().unwrap_or(0),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    models.sort_by(|a, b| a.name.cmp(&b.name));
    models
}

/// api0's own check: whether the key is accepted, and the AI credits left on
/// the account's plan this month.
pub(super) async fn probe_api0(cfg: &LlmConfig, key: &str) -> Result<String, String> {
    let url = format!("{}/balance", cfg.remote_base_url().trim_end_matches('/'));
    let resp = client(PROBE_TIMEOUT)?
        .get(&url)
        .bearer_auth(key)
        .headers(client_app(cfg.preset()))
        .send()
        .await
        .map_err(|e| format!("unreachable: {e}"))?;
    let status = resp.status();
    let text = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        return Err(format!("api0 returned {status}: {}", error_message(&text)));
    }
    let v: Value = serde_json::from_str(&text).map_err(|e| format!("bad reply: {e}"))?;
    api0_balance_line(&v)
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
                return Err(missing_key(preset));
            };
            if is_api0(preset) {
                return probe_api0(cfg, &key).await;
            }
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
