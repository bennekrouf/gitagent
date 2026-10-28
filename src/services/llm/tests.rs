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
    cfg.second.remote = "mistral".into();
    assert_eq!(
        cfg.second_config().remote_base_url(),
        "https://api.mistral.ai/v1"
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
fn a_region_is_api0_with_the_region_as_its_model() {
    let mut cfg = LlmConfig::default();
    assert_eq!(cfg.region(), None);
    cfg.use_region("china");
    assert_eq!(cfg.region().map(|r| r.id), Some("china"));
    assert_eq!(cfg.remote_model_name(), "china");
    assert_eq!(cfg.active_model(), REGIONS[1].shown());
    assert_eq!(REGIONS[1].display, "🇨🇳 China");
    // api0 through a proxy, or with a model named by hand, is not a region.
    cfg.remote_url = "https://proxy.example.com/v1".into();
    assert_eq!(cfg.region(), None);

    cfg.use_own_key();
    assert_eq!(cfg.remote, DEFAULT_REMOTE);
    assert!(cfg.remote_model.is_empty() && cfg.remote_url.is_empty());
}

#[test]
fn a_region_costs_what_a_typical_step_spends_rounded_up() {
    let v = json!({
        "object": "list",
        "tokens_per_credit": 10000,
        "data": [
            { "id": "europe", "multiplier": 1.0 },
            { "id": "usa", "multiplier": 4.0 },
            { "id": "default" }
        ]
    });
    let offers = parse_region_offers(&v);
    assert_eq!(offers.len(), 2, "only the regions");
    assert_eq!(offers[0].credits_per_step(), 1);
    assert_eq!(offers[1].credits_per_step(), 3);
}

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
    assert_eq!(remote("does-not-exist").key, DEFAULT_REMOTE);
}

#[test]
fn api0_is_offered_under_its_own_name_and_lets_the_server_pick_the_model() {
    let api0 = remote("api0");
    assert!(is_api0(api0));
    assert_eq!(api0.label, "api0");
    assert_eq!(api0.model, "default");
    assert!(!is_api0(remote("openai")));
}

#[test]
fn an_error_body_is_read_for_its_message() {
    let body = r#"{"error":{"message":"This month's AI credits are used up.","type":"insufficient_quota"}}"#;
    assert_eq!(error_message(body), "This month's AI credits are used up.");
    assert_eq!(error_message("plain text"), "plain text");
}

#[test]
fn the_api0_check_says_what_the_plan_has_left() {
    let ok = api0_balance_line(&json!({"plan": true, "balance": 420})).unwrap();
    assert!(ok.contains("420 AI credits"));
    assert!(api0_balance_line(&json!({"plan": false, "balance": 0}))
        .unwrap_err()
        .contains("no plan"));
    assert!(api0_balance_line(&json!({"plan": true, "balance": 0}))
        .unwrap_err()
        .contains("used up"));
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
    // The old shape named the provider directly and had its own url/model keys.
    let old = r#"{
            "kind": "DeepSeek",
            "ollama_url": "http://localhost:11434",
            "ollama_model": "qwen2.5-coder:14b",
            "ollama_num_ctx": 16384,
            "deepseek_url": "https://llm.example/v1",
            "deepseek_model": "chat-model"
        }"#;
    let cfg: LlmConfig = serde_json::from_str(old).expect("old settings must still parse");
    assert_eq!(cfg.kind, ProviderKind::Remote);
    assert_eq!(cfg.remote_base_url(), "https://llm.example/v1");
    assert_eq!(cfg.remote_model_name(), "chat-model");
}

#[test]
fn the_default_context_is_large_enough_for_a_real_diff() {
    // Guards against regressing to ollama's 4096 default.
    assert!(LlmConfig::default().ollama_num_ctx >= 16384);
}

#[test]
fn installed_models_are_read_by_name_with_their_size() {
    let reply = serde_json::json!({ "models": [
        { "name": "qwen2.5-coder:14b", "size": 8_988_124_298u64 },
        { "name": "openbmb/minicpm5-2b:latest", "size": 1_600_000_000u64 },
        { "size": 5 }
    ]});
    let models = parse_tags(&reply);
    assert_eq!(models.len(), 2, "an entry with no name is skipped");
    assert_eq!(
        models[0].name, "openbmb/minicpm5-2b:latest",
        "sorted by name"
    );
    assert_eq!(models[1].size_label(), "9.0 GB");
    assert!(parse_tags(&serde_json::json!({})).is_empty());
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

#[test]
fn api0_without_a_region_comes_back_on_the_first_one() {
    let cfg = LlmConfig {
        kind: ProviderKind::Remote,
        remote: "api0".into(),
        ..LlmConfig::default()
    }
    .with_region_filled();
    assert_eq!(cfg.region().map(|r| r.id), Some("europe"));
    assert_eq!(
        cfg.active_model(),
        if cfg!(windows) {
            "Europe"
        } else {
            "\u{1F1EA}\u{1F1FA} Europe"
        },
        "a button showing the model says where it runs, never \"default\""
    );
}

#[test]
fn a_model_named_by_hand_survives_the_region_fill_in() {
    let cfg = LlmConfig {
        kind: ProviderKind::Remote,
        remote: "api0".into(),
        remote_model: "a-model-api0-named".into(),
        ..LlmConfig::default()
    }
    .with_region_filled();
    assert_eq!(cfg.remote_model, "a-model-api0-named");
}

#[test]
fn the_second_reviewer_picks_a_region_of_its_own() {
    let mut cfg = LlmConfig::default();
    cfg.second.use_region("china");
    assert_eq!(cfg.second.region().map(|r| r.id), Some("china"));
    assert_eq!(
        cfg.second_config().active_model(),
        if cfg!(windows) {
            "China"
        } else {
            "\u{1F1E8}\u{1F1F3} China"
        }
    );
    cfg.second.use_own_key();
    assert!(
        cfg.second.region().is_none(),
        "a key of your own is not api0"
    );
    assert_eq!(cfg.second.remote, DEFAULT_REMOTE);
}
