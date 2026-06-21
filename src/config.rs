use anyhow::{Context, Result};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::Path;

/// The declared source of truth (`~/.config/ccstack/ccstack.toml`).
#[derive(Debug, Default, Deserialize)]
pub struct DeclaredConfig {
    #[serde(default)]
    pub global: Global,
    #[serde(default)]
    pub profiles: BTreeMap<String, Profile>,
    #[serde(default)]
    pub providers: BTreeMap<String, Provider>,
    #[serde(default)]
    pub opencode_local: OpenCodeLocal,
}

#[derive(Debug, Default, Deserialize)]
pub struct Global {
    /// `false` => render `CLAUDE_CODE_ATTRIBUTION_HEADER=0`.
    #[serde(default)]
    pub attribution_header: bool,
    /// Absolute path (or `~/...`) to the `headroom` binary. headroom-ai has a
    /// Rust/PyO3 core that caps at Python 3.13, so this must point at a 3.13
    /// venv — the MCP entry registers THIS path, not bare `headroom` on PATH
    /// (which won't resolve when the default python is 3.14+).
    #[serde(default)]
    pub headroom_bin: Option<String>,
    /// Write a `headroom_compress` usage rule into ~/.claude/CLAUDE.md (text_block).
    #[serde(default)]
    pub claude_md_rule: bool,
}

#[derive(Debug, Default, Deserialize)]
pub struct Profile {
    #[serde(default)]
    pub plan_model: Option<String>,
    #[serde(default)]
    pub exec_model: Option<String>,
    #[serde(default)]
    pub headroom: bool,
    #[serde(default)]
    pub headroom_mode: Option<String>,
    #[serde(default)]
    pub long_context_threshold: Option<u32>,
    /// The cloud↔local router (CCR) sets ANTHROPIC_AUTH_TOKEN/base-url, which
    /// DISABLES a Claude subscription's OAuth. Only emit it when this is true
    /// (i.e. you run Claude Code on a pay-per-token API key, not a subscription).
    #[serde(default)]
    pub api_key_billing: bool,
}

#[derive(Debug, Default, Deserialize)]
pub struct Provider {
    #[serde(default)]
    pub base_url: Option<String>,
    #[serde(default)]
    pub api_key: Option<String>,
}

/// The local OpenCode -> MTPLX Router -> mtplx stack (+ optional Headroom proxy hop).
/// ccstack renders the `mtplx` provider into opencode.json and wires plan/build.
#[derive(Debug, Default, Deserialize)]
pub struct OpenCodeLocal {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub router_url: Option<String>,
    #[serde(default)]
    pub planner_id: Option<String>,
    #[serde(default)]
    pub builder_id: Option<String>,
    #[serde(default)]
    pub planner_context: Option<u32>,
    #[serde(default)]
    pub builder_context: Option<u32>,
    /// MTPLX Router config.json — so ccstack can flip its compression-proxy toggle.
    #[serde(default)]
    pub router_config_path: Option<String>,
    /// Local Headroom compression hop. OFF by default — local benefit is speed/memory
    /// only ($0 tokens locally); it costs prefix-cache churn. Enable only for long sessions.
    #[serde(default)]
    pub headroom: bool,
    #[serde(default)]
    pub headroom_mode: Option<String>,
    #[serde(default)]
    pub headroom_upstream: Option<String>,
    #[serde(default)]
    pub headroom_port: Option<u32>,
}

impl DeclaredConfig {
    pub fn load(path: &Path) -> Result<Self> {
        let txt = std::fs::read_to_string(path)
            .with_context(|| format!("reading {}", path.display()))?;
        toml::from_str(&txt).context("parsing ccstack.toml")
    }

    pub fn sample() -> &'static str {
        SAMPLE
    }
}

pub const SAMPLE: &str = r#"# ccstack declared config (source of truth)
[global]
attribution_header = false        # -> CLAUDE_CODE_ATTRIBUTION_HEADER=0 (safe on a subscription)
# headroom-ai needs Python <=3.13 (Rust/PyO3 core). Point at a 3.13 venv so the
# MCP entry registers an absolute path, not bare `headroom` on PATH.
headroom_bin = "~/.headroom-venv/bin/headroom"
claude_md_rule = true             # write a headroom_compress usage rule into ~/.claude/CLAUDE.md

# Subscription (OAuth) users: safe, non-routing optimizations only.
# Headroom runs in MCP mode (compression exposed as tools), NOT as a proxy.
[profiles.cloud]
plan_model = "anthropic,claude-opus-4"
headroom = true
headroom_mode = "mcp"

# Cloud-plan / local-exec router (claude-code-router). It sets
# ANTHROPIC_AUTH_TOKEN/base-url, which DISABLES a Claude subscription's OAuth.
# Leave api_key_billing = false to stay subscription-safe; set true ONLY if you
# run Claude Code on a pay-per-token API key.
[profiles.hybrid]
plan_model = "anthropic,claude-opus-4"
exec_model = "omlx,Qwen3.6-35B-A3B-MLX-8bit"
long_context_threshold = 60000
api_key_billing = false
headroom = true
headroom_mode = "mcp"

# The local OpenCode -> MTPLX Router -> mtplx stack. ccstack writes the `mtplx`
# provider into opencode.json and wires plan->planner / build->builder.
[opencode_local]
enabled        = true
router_url     = "http://127.0.0.1:11435/v1"
planner_id     = "mtplx-qwen36-27b-optimized-quality"
builder_id     = "mtplx-qwen36-35b-a3b-optimized-speed"
planner_context = 65536
builder_context = 131072
router_config_path = "~/Library/Application Support/MTPLX Router/config.json"
# Local Headroom compression hop. OFF by default (speed/memory only; costs prefix-cache
# churn; $0 tokens locally). Flip to true ONLY for long, context-heavy sessions.
headroom          = false
headroom_mode     = "cache"        # cache (prefix-safe) | token
headroom_upstream = "http://127.0.0.1:8011/v1"
headroom_port     = 8787

[providers.omlx]
base_url = "http://127.0.0.1:1234/v1/chat/completions"
api_key  = "1234"
"#;
