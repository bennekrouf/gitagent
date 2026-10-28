//! Which model does what: the provider, the lenses, the second opinion.

use super::*;

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

impl SecondModel {
    /// The api0 region this reviewer is set to, if any. Same question as
    /// `LlmConfig::region`, asked of the second opinion's own choice.
    pub fn region(&self) -> Option<&'static Region> {
        if self.kind != ProviderKind::Remote || !is_api0(remote(&self.remote)) {
            return None;
        }
        REGIONS.iter().find(|r| r.id == self.remote_model.trim())
    }

    pub fn use_region(&mut self, id: &str) {
        self.kind = ProviderKind::Remote;
        self.remote = "api0".into();
        self.remote_model = id.to_string();
    }

    pub fn use_own_key(&mut self) {
        self.kind = ProviderKind::Remote;
        if is_api0(remote(&self.remote)) {
            self.remote = default_remote();
            self.remote_model.clear();
        }
    }
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

/// The part of the settings one open folder chooses for itself: which model
/// its steps call, and where that model runs.
///
/// Running one GitAgent window per folder is the ordinary way to work, and
/// the two windows need not agree — China in the folder holding one project,
/// the United States in another. Only the model is scoped this way. The
/// second opinion and the focused reviews are about *how* a review is done
/// rather than where, and nobody wants those to change with the folder.
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub struct ModelChoice {
    pub kind: ProviderKind,
    pub ollama_url: String,
    pub ollama_model: String,
    pub ollama_num_ctx: u32,
    #[serde(default = "default_remote")]
    pub remote: String,
    #[serde(default)]
    pub remote_url: String,
    #[serde(default)]
    pub remote_model: String,
}

impl LlmConfig {
    /// Just the per-folder part, to be stored against a folder.
    pub fn model_choice(&self) -> ModelChoice {
        ModelChoice {
            kind: self.kind,
            ollama_url: self.ollama_url.clone(),
            ollama_model: self.ollama_model.clone(),
            ollama_num_ctx: self.ollama_num_ctx,
            remote: self.remote.clone(),
            remote_url: self.remote_url.clone(),
            remote_model: self.remote_model.clone(),
        }
    }

    /// This config with a folder's model laid over it — the shared second
    /// opinion and lenses kept as they are.
    pub fn with_model_choice(&self, choice: &ModelChoice) -> Self {
        Self {
            kind: choice.kind,
            ollama_url: choice.ollama_url.clone(),
            ollama_model: choice.ollama_model.clone(),
            ollama_num_ctx: choice.ollama_num_ctx,
            remote: choice.remote.clone(),
            remote_url: choice.remote_url.clone(),
            remote_model: choice.remote_model.clone(),
            second: self.second.clone(),
            lenses: self.lenses.clone(),
        }
    }
}

impl LlmConfig {
    pub fn active_model(&self) -> &str {
        match self.kind {
            ProviderKind::Ollama => &self.ollama_model,
            ProviderKind::Remote => self
                .region()
                .map_or_else(|| self.remote_model_name(), |r| r.shown()),
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

    /// Settings written before api0 had regions name none, and api0's own
    /// model is then the word "default" — which is what every button showing
    /// the model would have to say. Put such a choice on the first region, so
    /// a window names where it runs instead. A model typed by hand is left
    /// alone: only an empty one is filled in.
    pub fn with_region_filled(mut self) -> Self {
        if self.kind == ProviderKind::Remote
            && is_api0(self.preset())
            && self.remote_url.trim().is_empty()
            && self.remote_model.trim().is_empty()
        {
            self.remote_model = REGIONS[0].id.to_string();
        }
        if self.second.kind == ProviderKind::Remote
            && is_api0(remote(&self.second.remote))
            && self.second.remote_model.trim().is_empty()
        {
            self.second.remote_model = REGIONS[0].id.to_string();
        }
        self
    }

    /// The api0 region this is set to, if any.
    pub fn region(&self) -> Option<&'static Region> {
        if self.kind != ProviderKind::Remote
            || !is_api0(self.preset())
            || !self.remote_url.trim().is_empty()
        {
            return None;
        }
        REGIONS.iter().find(|r| r.id == self.remote_model.trim())
    }

    /// One click in Settings: api0, in this region.
    pub fn use_region(&mut self, id: &str) {
        self.kind = ProviderKind::Remote;
        self.remote = "api0".into();
        self.remote_url.clear();
        self.remote_model = id.to_string();
    }

    /// A provider of the user's own, with their own key: the one they had,
    /// or the default when they were on api0.
    pub fn use_own_key(&mut self) {
        self.kind = ProviderKind::Remote;
        if is_api0(self.preset()) {
            self.remote = default_remote();
            self.remote_url.clear();
            self.remote_model.clear();
        }
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
        key_for(self.preset())
    }
}

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
                ((tokens * CHARS_PER_TOKEN) as usize).min(super::super::git::DIFF_CAP)
            }
            ProviderKind::Remote | ProviderKind::Off => super::super::git::DIFF_CAP,
        }
    }
}
