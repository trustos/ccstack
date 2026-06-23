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
    #[serde(default)]
    pub compress_hook: CompressHook,
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
    /// 3.13 venv ccstack creates (pkg_install) if missing, to host headroom-ai.
    #[serde(default)]
    pub headroom_venv: Option<String>,
    /// headroom-ai extras to install (pytorch-mps = the ML compressor for Apple Silicon).
    #[serde(default)]
    pub headroom_extras: Option<String>,
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

/// The local OpenCode -> MTPLX Router -> mtplx caching hop. The MTPLX Router owns
/// opencode.json (provider/agents — it holds the model list natively); ccstack's only job
/// here is CACHING: bring up Headroom and insert it between the router and mtplx.
#[derive(Debug, Default, Deserialize)]
pub struct OpenCodeLocal {
    #[serde(default)]
    pub enabled: bool,
    /// MTPLX Router config.json — so ccstack can flip its `compressionProxyURL` toggle to
    /// route the router through the Headroom cache.
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

/// Subscription-safe AUTOMATIC compression for Claude Code: a PostToolUse hook that
/// pipes large tool outputs through Headroom (locally, no proxy / no ANTHROPIC_BASE_URL,
/// so it never touches OAuth) and returns `updatedToolOutput` so the model sees fewer
/// tokens. Closes the gap that Headroom's MCP is on-demand-only and its proxy breaks a
/// subscription's OAuth. Installs a generated hook script (file_create) + registers it in
/// ~/.claude/settings.json `hooks.PostToolUse` (json_key) — both reversible in the ledger.
#[derive(Debug, Default, Deserialize)]
pub struct CompressHook {
    #[serde(default)]
    pub enabled: bool,
    /// Skip compression for tool outputs below this rough token count (cheap pre-check
    /// runs BEFORE importing headroom, so small/fast tool calls pay no penalty). Default 1500.
    #[serde(default)]
    pub min_tokens: Option<u32>,
    /// Claude Code hook `matcher` — which tools to compress. Default "Bash|WebFetch"
    /// (verbose + rarely needed verbatim). Add Read/Grep/Glob at your own risk (the model
    /// often needs exact lines/paths from those). Never include Edit/Write.
    #[serde(default)]
    pub tools: Option<String>,
    /// Headroom Kompress (ML) model. "disabled" (default) = structural compression only
    /// (SmartCrusher + CacheAligner: dedup logs/JSON; fast, offline, no model load per call).
    /// Set a HuggingFace id to enable ML prose compression (slower; loads a model).
    #[serde(default)]
    pub kompress_model: Option<String>,
    /// Where the hook stashes the full original (for lossless recovery via `Read`).
    /// Default "~/.config/ccstack/originals".
    #[serde(default)]
    pub originals_dir: Option<String>,
    /// Unix socket the warm compression daemon listens on (and the hook connects to). The
    /// daemon loads the Headroom model once and stays warm, so the per-call hook never reloads
    /// it. Default "~/.config/ccstack/compress-daemon.sock".
    #[serde(default)]
    pub socket: Option<String>,
}

impl DeclaredConfig {
    pub fn load(path: &Path) -> Result<Self> {
        let txt =
            std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
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
headroom_venv = "~/.headroom-venv"          # ccstack creates this 3.13 venv if missing (pkg_install)
headroom_extras = "mcp,proxy,pytorch-mps,code"  # extras: mcp (Claude Code tools) + proxy (local cache-hop server) + pytorch-mps (ML compressor)

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

# Local caching hop for OpenCode -> MTPLX Router -> mtplx. The MTPLX Router owns
# opencode.json (its menu "Write OpenCode config" generates the canonical mtplx provider);
# ccstack only inserts the Headroom cache between the router and mtplx.
[opencode_local]
enabled        = true
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

# Subscription-safe AUTOMATIC compression. Headroom's MCP is on-demand-only and its proxy
# breaks subscription OAuth — this fills the gap with a Claude Code PostToolUse hook that
# compresses large tool outputs locally (no proxy, no ANTHROPIC_BASE_URL) and returns
# updatedToolOutput. A launchd daemon loads the Headroom model ONCE and stays warm; the hook is
# a thin Unix-socket client (sub-100ms/call, no per-call model reload). `apply` installs the
# daemon script + plist + launchd service + the hook script + its settings.json registration —
# all reversible via the ledger. Needs Claude Code >= 2.1.121.
[compress_hook]
enabled        = false             # flip to true, then `ccstack apply`
min_tokens     = 1500              # only compress tool outputs bigger than this (cheap pre-check)
tools          = "Bash|WebFetch"   # matcher: which tools to compress (add Read/Grep at your own risk)
# kompress_model unset = Headroom's default ML text compressor (compresses logs/dumps ~40-65%).
# The warm daemon absorbs the model-load cost ONCE, so per-call latency stays low. Set
# "disabled" for structural-only (no ML), or a HuggingFace id for a domain-specific model.
# kompress_model = "disabled"
originals_dir  = "~/.config/ccstack/originals"          # full originals stashed here for lossless Read-recovery
# socket       = "~/.config/ccstack/compress-daemon.sock"  # daemon socket (default shown)
"#;
