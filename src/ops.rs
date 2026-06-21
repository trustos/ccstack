use anyhow::{Context, Result};
use serde_json::Value;
use std::path::{Path, PathBuf};

use crate::config::DeclaredConfig;
use crate::render;
use crate::state::{Change, ChangeKind, Ledger, Status};
use crate::util;

fn config_path() -> Result<PathBuf> {
    Ok(util::ccstack_dir()?.join("ccstack.toml"))
}

/// Write a sample declared config if none exists.
pub fn init() -> Result<()> {
    let p = config_path()?;
    if p.exists() {
        println!("ccstack.toml already exists: {}", p.display());
        return Ok(());
    }
    if let Some(parent) = p.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&p, DeclaredConfig::sample())?;
    println!("wrote sample config -> {}", p.display());
    println!("edit it, then run `ccstack plan` and `ccstack apply`.");
    Ok(())
}

/// A change derived from the declared config, ready to apply.
struct Planned {
    profile: String,
    kind: ChangeKind,
    target: String,
    key_path: Option<String>,
    value: Option<Value>,
    contents: Option<String>,
    applied_value: Option<String>,
}

fn plan_changes(cfg: &DeclaredConfig) -> Vec<Planned> {
    let mut out = Vec::new();

    // global: attribution header off => env.CLAUDE_CODE_ATTRIBUTION_HEADER = "0"
    if !cfg.global.attribution_header {
        out.push(Planned {
            profile: "global".into(),
            kind: ChangeKind::JsonKey,
            target: "~/.claude/settings.json".into(),
            key_path: Some("env.CLAUDE_CODE_ATTRIBUTION_HEADER".into()),
            value: Some(Value::String("0".into())),
            contents: None,
            applied_value: Some("0".into()),
        });
    }

    // hybrid profile: executor subagent + claude-code-router config.
    // CCR sets ANTHROPIC_AUTH_TOKEN/base-url, which DISABLES a Claude
    // subscription's OAuth — so only emit these when API-key billing is declared.
    if let Some(h) = cfg.profiles.get("hybrid") {
        if h.api_key_billing {
            if let Some(exec) = &h.exec_model {
                out.push(Planned {
                    profile: "hybrid".into(),
                    kind: ChangeKind::FileCreate,
                    target: "~/.claude/agents/executor.md".into(),
                    key_path: None,
                    value: None,
                    contents: Some(executor_agent(exec)),
                    applied_value: None,
                });
            }
            out.push(Planned {
                profile: "hybrid".into(),
                kind: ChangeKind::FileCreate,
                target: "~/.claude-code-router/config.json".into(),
                key_path: None,
                value: None,
                contents: Some(router_config(cfg, h)),
                applied_value: None,
            });
        }
    }

    // headroom MCP (subscription-safe): register the compression tools so
    // apply/revert covers it. Matches `headroom mcp install`'s target file.
    let wants_headroom_mcp = cfg
        .profiles
        .values()
        .any(|p| p.headroom && p.headroom_mode.as_deref() != Some("proxy"));
    if wants_headroom_mcp {
        // Ensure the 3.13 headroom venv exists (pkg_install no-ops if already present).
        let venv = cfg
            .global
            .headroom_venv
            .clone()
            .unwrap_or_else(|| "~/.headroom-venv".to_string());
        let extras = cfg
            .global
            .headroom_extras
            .clone()
            .unwrap_or_else(|| "mcp,pytorch-mps,code".to_string());
        out.push(Planned {
            profile: "headroom".into(),
            kind: ChangeKind::PkgInstall,
            target: venv,
            key_path: None,
            value: None,
            contents: Some(format!("headroom-ai[{}]", extras)),
            applied_value: None,
        });
        // headroom-ai needs Python <=3.13, so register the ABSOLUTE path to a
        // 3.13-venv `headroom` rather than bare `headroom` on PATH (which won't
        // resolve when the default python is 3.14+).
        let bin = cfg.global.headroom_bin.as_deref().unwrap_or("headroom");
        let cmd = util::expand_tilde(bin)
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_else(|_| bin.to_string());
        out.push(Planned {
            profile: "headroom".into(),
            kind: ChangeKind::JsonKey,
            target: "~/.claude/mcp.json".into(),
            key_path: Some("mcpServers.headroom".into()),
            value: Some(serde_json::json!({"command": cmd, "args": ["mcp", "serve"]})),
            contents: None,
            applied_value: Some(format!("{} mcp serve", cmd)),
        });
    }

    // CLAUDE.md headroom_compress rule (text_block). Subscription MCP is on-demand,
    // so nudge Claude to actually call the tool on large dumps.
    if cfg.global.claude_md_rule && wants_headroom_mcp {
        out.push(Planned {
            profile: "headroom".into(),
            kind: ChangeKind::TextBlock,
            target: "~/.claude/CLAUDE.md".into(),
            key_path: Some("headroom".into()),
            value: None,
            contents: Some(CLAUDE_MD_RULE.to_string()),
            applied_value: None,
        });
    }

    // Local stack: OpenCode -> MTPLX Router -> mtplx (+ optional Headroom proxy hop).
    let oc = &cfg.opencode_local;
    if oc.enabled {
        // NB: opencode.json (provider / agents / plugin) is owned by the MTPLX Router, not
        // ccstack — the router holds the model list natively and writes its own OpenCode
        // provider. ccstack's only role in the local path is CACHING: inserting the Headroom
        // hop (router -> headroom:port -> mtplx).
        if oc.headroom {
            let port = oc.headroom_port.unwrap_or(8787);
            if let Some(rc) = &oc.router_config_path {
                out.push(Planned {
                    profile: "headroom".into(),
                    kind: ChangeKind::JsonKey,
                    target: rc.clone(),
                    key_path: Some("compressionProxyURL".into()),
                    value: Some(Value::String(format!("http://127.0.0.1:{}/v1", port))),
                    contents: None,
                    applied_value: None,
                });
            }
            out.push(Planned {
                profile: "headroom".into(),
                kind: ChangeKind::FileCreate,
                target: "~/Library/LaunchAgents/com.ccstack.headroom-proxy.plist".into(),
                key_path: None,
                value: None,
                contents: Some(headroom_plist(cfg, oc, port)),
                applied_value: None,
            });
            let plist = "$HOME/Library/LaunchAgents/com.ccstack.headroom-proxy.plist";
            out.push(Planned {
                profile: "headroom".into(),
                kind: ChangeKind::Service,
                target: "headroom-proxy".into(),
                key_path: None,
                value: None,
                contents: Some(format!(
                    "launchctl unload {p} 2>/dev/null; launchctl load -w {p}",
                    p = plist
                )),
                applied_value: Some(format!("launchctl unload -w {}", plist)),
            });
        }
    }

    out
}

const CLAUDE_MD_RULE: &str = "## Headroom compression\n\nBefore reasoning over a large file read, log, or tool dump, compress it with the `headroom_compress` MCP tool and keep the returned hash; call `headroom_retrieve` if you later need the original. Keeps context small without losing information.";

/// A launchd plist that runs `headroom proxy` pointed at mtplx (compression-only, cache mode).
fn headroom_plist(cfg: &DeclaredConfig, oc: &crate::config::OpenCodeLocal, port: u32) -> String {
    let bin_raw = cfg.global.headroom_bin.as_deref().unwrap_or("headroom");
    let bin = util::expand_tilde(bin_raw)
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|_| bin_raw.to_string());
    let mode = oc
        .headroom_mode
        .clone()
        .unwrap_or_else(|| "cache".to_string());
    let upstream = oc
        .headroom_upstream
        .clone()
        .unwrap_or_else(|| "http://127.0.0.1:8011/v1".to_string());
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
  <key>Label</key><string>com.ccstack.headroom-proxy</string>
  <key>ProgramArguments</key><array>
    <string>{bin}</string><string>proxy</string>
    <string>--port</string><string>{port}</string>
    <string>--mode</string><string>{mode}</string>
    <string>--no-ccr-inject-tool</string><string>--no-ccr-marker</string>
  </array>
  <key>EnvironmentVariables</key><dict>
    <key>OPENAI_TARGET_API_URL</key><string>{upstream}</string>
    <key>OPENAI_API_KEY</key><string>local</string>
    <key>HEADROOM_SKIP_UPSTREAM_CHECK</key><string>1</string>
  </dict>
  <key>RunAtLoad</key><true/>
  <key>KeepAlive</key><true/>
  <key>StandardOutPath</key><string>/tmp/ccstack-headroom-proxy.log</string>
  <key>StandardErrorPath</key><string>/tmp/ccstack-headroom-proxy.log</string>
</dict></plist>
"#,
        bin = bin,
        port = port,
        mode = mode,
        upstream = upstream
    )
}

fn executor_agent(exec_model: &str) -> String {
    format!(
        "---\ndescription: Implements the plan — edits, runs tests/commands, reads code.\n---\n<CCR-SUBAGENT-MODEL>{}</CCR-SUBAGENT-MODEL>\n\nYou are the implementer. Carry out the approved plan precisely; be concise — report results, not reasoning.\n",
        exec_model
    )
}

fn split_pm(s: &str) -> (String, String) {
    match s.split_once(',') {
        Some((p, m)) => (p.trim().to_string(), m.trim().to_string()),
        None => ("anthropic".to_string(), s.trim().to_string()),
    }
}

/// Render a claude-code-router config.json for the hybrid profile:
/// planning/think on the cloud model; execution/background/long-context on local.
fn router_config(cfg: &DeclaredConfig, h: &crate::config::Profile) -> String {
    let plan = h
        .plan_model
        .clone()
        .unwrap_or_else(|| "anthropic,claude-opus-4".to_string());
    let exec = h
        .exec_model
        .clone()
        .unwrap_or_else(|| "omlx,local".to_string());
    let (plan_p, plan_m) = split_pm(&plan);
    let (exec_p, exec_m) = split_pm(&exec);
    let threshold = h.long_context_threshold.unwrap_or(60000);

    let omlx = cfg.providers.get("omlx");
    let omlx_url = omlx
        .and_then(|p| p.base_url.clone())
        .unwrap_or_else(|| "http://127.0.0.1:1234/v1/chat/completions".to_string());
    let omlx_key = omlx
        .and_then(|p| p.api_key.clone())
        .unwrap_or_else(|| "1234".to_string());

    let doc = serde_json::json!({
        "LOG": true,
        "Providers": [
            {
                "name": plan_p,
                "api_base_url": "https://api.anthropic.com/v1/messages",
                "api_key": "REPLACE_WITH_ANTHROPIC_API_KEY",
                "models": [plan_m]
            },
            {
                "name": exec_p,
                "api_base_url": omlx_url,
                "api_key": omlx_key,
                "models": [exec_m]
            }
        ],
        "Router": {
            "default": plan.clone(),
            "think": plan.clone(),
            "background": exec.clone(),
            "longContext": exec.clone(),
            "longContextThreshold": threshold
        }
    });
    serde_json::to_string_pretty(&doc).unwrap_or_default()
}

pub fn apply(dry: bool) -> Result<()> {
    let cfg = DeclaredConfig::load(&config_path()?)
        .context("no ccstack.toml — run `ccstack init` first")?;
    let mut ledger = Ledger::load()?;
    let txn = format!("apply_{}", util::now_ts());
    let planned = plan_changes(&cfg);

    if dry {
        println!("plan ({} candidate change(s)):", planned.len());
    }

    for pc in planned {
        let already = ledger
            .active()
            .any(|c| c.target == pc.target && c.key_path == pc.key_path);
        if already {
            if dry {
                println!("  = {} (already applied)", pc.target);
            }
            continue;
        }

        let target = util::expand_tilde(&pc.target)?;
        let (prior, region_hash) = match pc.kind {
            ChangeKind::JsonKey => render::apply_json_key(
                &target,
                pc.key_path.as_deref().unwrap_or_default(),
                pc.value.clone().unwrap_or(Value::Null),
                dry,
            )?,
            ChangeKind::FileCreate => {
                render::apply_file_create(&target, pc.contents.as_deref().unwrap_or(""), dry)?
            }
            ChangeKind::TextBlock => render::apply_text_block(
                &target,
                pc.key_path.as_deref().unwrap_or_default(),
                pc.contents.as_deref().unwrap_or(""),
                dry,
            )?,
            ChangeKind::Service => {
                render::apply_service(pc.contents.as_deref().unwrap_or(""), dry)?
            }
            ChangeKind::PkgInstall => {
                render::apply_pkg_install(&target, pc.contents.as_deref().unwrap_or(""), dry)?
            }
        };

        if dry {
            continue;
        }

        let change = Change {
            id: ledger.next_id(),
            txn: txn.clone(),
            profile: pc.profile,
            kind: pc.kind,
            target: pc.target,
            key_path: pc.key_path,
            prior,
            applied_value: pc.applied_value,
            region_hash,
            status: Status::Applied,
        };
        println!("  applied {} ({})", change.id, change.target);
        ledger.changes.push(change);
    }

    if !dry {
        ledger.save()?;
        println!("ledger updated ({} entries)", ledger.changes.len());
    }
    Ok(())
}

fn sync_state(c: &Change, target: &Path) -> Result<&'static str> {
    match &c.kind {
        ChangeKind::JsonKey => {
            let key = c.key_path.as_deref().unwrap_or_default();
            Ok(match render::current_json_key_hash(target, key)? {
                None => "missing",
                Some(h) if h == c.region_hash => "in-sync",
                Some(_) => "drifted",
            })
        }
        ChangeKind::FileCreate => Ok(match render::current_file_hash(target)? {
            None => "missing",
            Some(h) if h == c.region_hash => "in-sync",
            Some(_) => "drifted",
        }),
        ChangeKind::TextBlock => {
            let marker = c.key_path.as_deref().unwrap_or_default();
            Ok(match render::current_text_block_hash(target, marker)? {
                None => "missing",
                Some(h) if h == c.region_hash => "in-sync",
                Some(_) => "drifted",
            })
        }
        ChangeKind::PkgInstall => Ok(match render::current_pkg_install(target)? {
            None => "missing",
            Some(_) => "in-sync",
        }),
        ChangeKind::Service => Ok("n/a"),
    }
}

pub fn status() -> Result<()> {
    let ledger = Ledger::load()?;
    if ledger.active().next().is_none() {
        println!("no changes applied yet.");
        return Ok(());
    }
    println!(
        "{:<8} {:<8} {:<11} {:<9} target",
        "id", "profile", "kind", "sync"
    );
    for c in ledger.active() {
        let target = util::expand_tilde(&c.target)?;
        let sync = sync_state(c, &target)?;
        println!(
            "{:<8} {:<8} {:<11} {:<9} {}",
            c.id,
            c.profile,
            format!("{:?}", c.kind),
            sync,
            c.target
        );
    }
    Ok(())
}

pub fn verify() -> Result<()> {
    let ledger = Ledger::load()?;
    let mut bad = 0;
    for c in ledger.active() {
        let target = util::expand_tilde(&c.target)?;
        match sync_state(c, &target)? {
            "in-sync" => {}
            other => {
                bad += 1;
                println!("  {} {} -> {}", other, c.id, c.target);
            }
        }
    }
    if bad == 0 {
        println!("verify: all applied changes in sync.");
        Ok(())
    } else {
        anyhow::bail!("verify: {} change(s) drifted or missing", bad)
    }
}

pub fn revert(all: bool, profile: Option<String>, change: Option<String>) -> Result<()> {
    let mut ledger = Ledger::load()?;
    let mut selected: Vec<usize> = ledger
        .changes
        .iter()
        .enumerate()
        .filter(|(_, c)| c.status == Status::Applied)
        .filter(|(_, c)| {
            all || profile.as_deref() == Some(c.profile.as_str())
                || change.as_deref() == Some(c.id.as_str())
        })
        .map(|(i, _)| i)
        .collect();
    selected.reverse(); // LIFO

    if selected.is_empty() {
        println!("nothing to revert for that selector.");
        return Ok(());
    }

    for i in selected {
        let c = ledger.changes[i].clone();
        let target = util::expand_tilde(&c.target)?;
        if sync_state(&c, &target)? == "drifted" {
            println!(
                "  ! {} drifted at {} — skipping (resolve manually)",
                c.id, c.target
            );
            continue;
        }
        match &c.kind {
            ChangeKind::JsonKey => render::revert_json_key(
                &target,
                c.key_path.as_deref().unwrap_or_default(),
                &c.prior,
            )?,
            ChangeKind::FileCreate => render::revert_file_create(&target, &c.prior)?,
            ChangeKind::TextBlock => render::revert_text_block(
                &target,
                c.key_path.as_deref().unwrap_or_default(),
                &c.prior,
            )?,
            ChangeKind::Service => {
                render::revert_service(c.applied_value.as_deref().unwrap_or_default())?
            }
            ChangeKind::PkgInstall => render::revert_pkg_install(&target, &c.prior)?,
        }
        ledger.changes[i].status = Status::Reverted;
        println!("  reverted {} ({})", c.id, c.target);
    }
    ledger.save()?;
    Ok(())
}
