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
            // `proxy` is required for the local OpenCode→mtplx cache hop (`headroom proxy`);
            // without it the launchd proxy crash-loops "Proxy dependencies not installed".
            .unwrap_or_else(|| "mcp,proxy,pytorch-mps,code".to_string());
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
            // Claude Code reads user-scoped MCP servers from ~/.claude.json (top-level
            // `mcpServers` = user scope, loads in all projects). It does NOT read
            // ~/.claude/mcp.json — writing there leaves the server silently inactive.
            target: "~/.claude.json".into(),
            key_path: Some("mcpServers.headroom".into()),
            value: Some(
                serde_json::json!({"type": "stdio", "command": cmd, "args": ["mcp", "serve"]}),
            ),
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

    // Subscription-safe AUTOMATIC compression (warm-daemon architecture). Headroom's MCP is
    // on-demand-only and its proxy breaks subscription OAuth; this fills the gap with a Claude
    // Code PostToolUse hook that compresses large tool outputs locally (no proxy / no
    // ANTHROPIC_BASE_URL → never touches OAuth). A launchd daemon loads the Headroom model ONCE
    // and stays warm; the hook is a thin Unix-socket client → sub-100ms/call instead of a
    // multi-second model reload every call. Five reversible changes (file_create ×3 + service +
    // json_key), all recorded in the ledger.
    let ch = &cfg.compress_hook;
    if ch.enabled {
        let min_tokens = ch.min_tokens.unwrap_or(1500);
        let tools = ch
            .tools
            .clone()
            .unwrap_or_else(|| "Bash|WebFetch".to_string());
        let originals = ch
            .originals_dir
            .clone()
            .unwrap_or_else(|| "~/.config/ccstack/originals".to_string());
        let socket = ch
            .socket
            .clone()
            .unwrap_or_else(|| "~/.config/ccstack/compress-daemon.sock".to_string());
        let py = headroom_python(cfg);
        let abs = |t: &str| {
            util::expand_tilde(t)
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_else(|_| t.to_string())
        };
        let daemon_target = "~/.config/ccstack/hooks/compress-daemon.py";
        let hook_target = "~/.config/ccstack/hooks/compress-tool-output.py";
        let plist_target = "~/Library/LaunchAgents/com.ccstack.compress-daemon.plist";

        // 1) warm daemon: imports headroom once, holds the model, serves compress over a socket.
        out.push(Planned {
            profile: "headroom".into(),
            kind: ChangeKind::FileCreate,
            target: daemon_target.into(),
            key_path: None,
            value: None,
            contents: Some(compress_daemon_script(
                ch.kompress_model.as_deref(),
                &socket,
            )),
            applied_value: None,
        });
        // 2) launchd plist (absolute paths — launchd does not expand ~).
        out.push(Planned {
            profile: "headroom".into(),
            kind: ChangeKind::FileCreate,
            target: plist_target.into(),
            key_path: None,
            value: None,
            contents: Some(compress_daemon_plist(&py, &abs(daemon_target))),
            applied_value: None,
        });
        // 3) load the daemon (reverse = unload).
        let plist = "$HOME/Library/LaunchAgents/com.ccstack.compress-daemon.plist";
        out.push(Planned {
            profile: "headroom".into(),
            kind: ChangeKind::Service,
            target: "compress-daemon".into(),
            key_path: None,
            value: None,
            contents: Some(format!(
                "launchctl unload {p} 2>/dev/null; launchctl load -w {p}",
                p = plist
            )),
            applied_value: Some(format!("launchctl unload -w {}", plist)),
        });
        // 4) the thin hook client (no model load — talks to the daemon; passthrough if it's down).
        out.push(Planned {
            profile: "headroom".into(),
            kind: ChangeKind::FileCreate,
            target: hook_target.into(),
            key_path: None,
            value: None,
            contents: Some(compress_hook_client_script(min_tokens, &socket, &originals)),
            applied_value: None,
        });
        // 5) register the hook (command needs ABSOLUTE paths — Claude Code does not expand ~).
        let command = format!("{} {}", py, abs(hook_target));
        out.push(Planned {
            profile: "headroom".into(),
            kind: ChangeKind::JsonKey,
            target: "~/.claude/settings.json".into(),
            key_path: Some("hooks.PostToolUse".into()),
            value: Some(serde_json::json!([
                {"matcher": tools, "hooks": [{"type": "command", "command": command}]}
            ])),
            contents: None,
            applied_value: Some(command),
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

/// Absolute path to the headroom-venv python — the compression hook does `import headroom`,
/// so it must run under that venv. Prefers `[global].headroom_venv`/bin/python, else derives
/// from `headroom_bin` (…/bin/headroom → …/bin/python), else falls back to `python3`.
fn headroom_python(cfg: &DeclaredConfig) -> String {
    if let Some(venv) = &cfg.global.headroom_venv {
        if let Ok(p) = util::expand_tilde(venv) {
            return p.join("bin/python").to_string_lossy().into_owned();
        }
    }
    if let Some(bin) = &cfg.global.headroom_bin {
        if let Ok(p) = util::expand_tilde(bin) {
            if let Some(parent) = p.parent() {
                return parent.join("python").to_string_lossy().into_owned();
            }
        }
    }
    "python3".to_string()
}

/// Python literal for the kompress model: unset/"default" => None (Headroom's default ML text
/// compressor — the path that actually compresses logs/dumps); "disabled" => structural-only
/// (fast, no ML load, but rarely compresses prose); a HF id => that model.
fn kompress_literal(kompress_model: Option<&str>) -> String {
    match kompress_model {
        None => "None".to_string(),
        Some(s) if s.is_empty() || s.eq_ignore_ascii_case("default") => "None".to_string(),
        Some(s) => format!("{s:?}"),
    }
}

/// The warm compression daemon (Python). Runs under the headroom-venv python via launchd:
/// imports headroom ONCE, pre-warms the model, and serves "compress this text" requests over a
/// Unix socket so the per-call hook never reloads the model. One request per connection: the
/// client sends a JSON body then half-closes; the daemon reads to EOF, compresses, replies JSON.
fn compress_daemon_script(kompress_model: Option<&str>, socket: &str) -> String {
    const TPL: &str = r#"#!/usr/bin/env python3
# ccstack-managed warm compression daemon — do NOT edit by hand (change ccstack.toml + re-apply).
# Loads Headroom's compressor ONCE (model stays warm) and serves compress requests over a Unix
# socket, so the PostToolUse hook is a thin, fast client (no per-call model reload).
import os, sys, json, socket, threading, signal

SOCKET_PATH = os.path.expanduser("__SOCKET__")
KOMPRESS_MODEL = __KOMPRESS_MODEL__

_lock = threading.Lock()
_compress = None
_CompressConfig = None

def _cfg(min_tokens):
    return _CompressConfig(
        compress_user_messages=True,
        protect_recent=0,
        protect_analysis_context=True,
        min_tokens_to_compress=min_tokens,
        kompress_model=KOMPRESS_MODEL,
    )

def _warm():
    global _compress, _CompressConfig
    from headroom.compress import compress as c, CompressConfig as cc
    _compress, _CompressConfig = c, cc
    try:  # force the model to load now so the first real request is fast
        _compress([{"role": "user", "content": "warmup " * 300}], config=_cfg(50))
    except Exception:
        pass

def _compressed_text(messages):
    out = ""
    for m in messages:
        c = m.get("content")
        if isinstance(c, str):
            out += c
        elif isinstance(c, list):
            for b in c:
                if isinstance(b, dict) and isinstance(b.get("text"), str):
                    out += b["text"]
    return out

def _handle(conn):
    try:
        chunks = []
        while True:
            b = conn.recv(65536)
            if not b:
                break
            chunks.append(b)
        raw = b"".join(chunks)
        if not raw:
            return
        req = json.loads(raw.decode("utf-8", "replace"))
        text = req.get("text", "")
        min_tokens = int(req.get("min_tokens", 1500))
        with _lock:
            res = _compress([{"role": "user", "content": text}], config=_cfg(min_tokens))
        resp = {
            "compressed": _compressed_text(res.messages),
            "tokens_before": res.tokens_before,
            "tokens_after": res.tokens_after,
            "tokens_saved": res.tokens_saved,
        }
    except Exception as e:
        resp = {"error": "%s: %s" % (type(e).__name__, e)}
    try:
        conn.sendall(json.dumps(resp).encode("utf-8"))
    except Exception:
        pass
    finally:
        conn.close()

def main():
    try:
        os.makedirs(os.path.dirname(SOCKET_PATH), exist_ok=True)
    except Exception:
        pass
    if os.path.exists(SOCKET_PATH):
        try:
            os.unlink(SOCKET_PATH)
        except OSError:
            pass
    _warm()  # load the model BEFORE binding, so a bound socket means "ready"
    srv = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    srv.bind(SOCKET_PATH)
    try:
        os.chmod(SOCKET_PATH, 0o600)
    except OSError:
        pass
    srv.listen(16)

    def _bye(*_a):
        try:
            os.unlink(SOCKET_PATH)
        except OSError:
            pass
        os._exit(0)

    signal.signal(signal.SIGTERM, _bye)
    signal.signal(signal.SIGINT, _bye)
    sys.stderr.write("compress-daemon ready on %s\n" % SOCKET_PATH)
    sys.stderr.flush()
    while True:
        try:
            conn, _ = srv.accept()
        except OSError:
            break
        threading.Thread(target=_handle, args=(conn,), daemon=True).start()

if __name__ == "__main__":
    main()
"#;
    TPL.replace("__SOCKET__", socket)
        .replace("__KOMPRESS_MODEL__", &kompress_literal(kompress_model))
}

/// launchd plist for the warm compression daemon (RunAtLoad + KeepAlive). Absolute paths only —
/// launchd does not expand `~`. `bin` is the headroom-venv python; `script` the daemon's path.
fn compress_daemon_plist(bin: &str, script: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
  <key>Label</key><string>com.ccstack.compress-daemon</string>
  <key>ProgramArguments</key><array>
    <string>{bin}</string><string>{script}</string>
  </array>
  <key>RunAtLoad</key><true/>
  <key>KeepAlive</key><true/>
  <key>StandardOutPath</key><string>/tmp/ccstack-compress-daemon.log</string>
  <key>StandardErrorPath</key><string>/tmp/ccstack-compress-daemon.log</string>
</dict></plist>
"#,
        bin = bin,
        script = script
    )
}

/// The PostToolUse hook (Python) — a THIN CLIENT to the warm daemon. A cheap size-gate runs
/// first (small/fast tool calls pay nothing); large outputs are sent to the daemon over the
/// socket and the compressed text is returned as `updatedToolOutput`. The full original is
/// stashed in ORIGINALS_DIR for lossless `Read`-recovery. Any error (incl. daemon down) →
/// passthrough (exit 0) so a tool result is never broken.
fn compress_hook_client_script(min_tokens: u32, socket: &str, originals_dir: &str) -> String {
    const TPL: &str = r#"#!/usr/bin/env python3
# ccstack-managed — do NOT edit by hand. Change [compress_hook] in ccstack.toml and re-apply.
# Claude Code PostToolUse hook: thin client to the warm Headroom compress daemon. Local only,
# no proxy / no ANTHROPIC_BASE_URL -> subscription-safe. Returns updatedToolOutput so the model
# sees fewer tokens; stashes the full original for lossless Read-recovery.
import sys, json, os, socket, hashlib

SOCKET_PATH = os.path.expanduser("__SOCKET__")
MIN_TOKENS = __MIN_TOKENS__
ORIGINALS_DIR = os.path.expanduser("__ORIGINALS_DIR__")

def _text(resp):
    if resp is None:
        return ""
    if isinstance(resp, str):
        return resp
    if isinstance(resp, dict):
        for k in ("text", "content", "output", "result", "stdout"):
            v = resp.get(k)
            if isinstance(v, str) and v:
                if k == "stdout":
                    err = resp.get("stderr")
                    return v + ("\n" + err if isinstance(err, str) and err else "")
                return v
        return json.dumps(resp, ensure_ascii=False)
    if isinstance(resp, list):
        parts = []
        for b in resp:
            if isinstance(b, dict) and isinstance(b.get("text"), str):
                parts.append(b["text"])
            elif isinstance(b, str):
                parts.append(b)
        return "\n".join(parts) if parts else json.dumps(resp, ensure_ascii=False)
    return str(resp)

def _ask_daemon(text):
    s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    s.settimeout(60)
    s.connect(SOCKET_PATH)
    s.sendall(json.dumps({"text": text, "min_tokens": MIN_TOKENS}).encode("utf-8"))
    s.shutdown(socket.SHUT_WR)
    chunks = []
    while True:
        b = s.recv(65536)
        if not b:
            break
        chunks.append(b)
    s.close()
    return json.loads(b"".join(chunks).decode("utf-8", "replace"))

def main():
    try:
        data = json.load(sys.stdin)
    except Exception:
        return 0
    resp = data.get("tool_response")
    if resp is None:
        resp = data.get("tool_output")
    if resp is None:
        resp = data.get("tool_result")
    text = _text(resp)
    if len(text) < MIN_TOKENS * 4:  # cheap gate; tiny outputs never hit the daemon
        return 0
    try:
        r = _ask_daemon(text)
    except Exception:
        return 0  # daemon down/unreachable -> passthrough (never block the tool)
    comp = r.get("compressed", "")
    if r.get("error") or not comp or r.get("tokens_saved", 0) <= 0 or len(comp) >= len(text):
        return 0  # no real benefit -> passthrough
    h = hashlib.sha256(text.encode("utf-8", "replace")).hexdigest()[:12]
    try:
        os.makedirs(ORIGINALS_DIR, exist_ok=True)
        path = os.path.join(ORIGINALS_DIR, h + ".txt")
        with open(path, "w") as f:
            f.write(text)
        note = "\n\n[ccstack-headroom: compressed ~%d->%d tokens. Full original: Read %s]" % (
            r.get("tokens_before", 0), r.get("tokens_after", 0), path)
    except Exception:
        note = "\n\n[ccstack-headroom: compressed output]"
    print(json.dumps({"hookSpecificOutput": {
        "hookEventName": "PostToolUse",
        "updatedToolOutput": comp + note,
    }}))
    return 0

if __name__ == "__main__":
    sys.exit(main())
"#;
    TPL.replace("__SOCKET__", socket)
        .replace("__MIN_TOKENS__", &min_tokens.to_string())
        .replace("__ORIGINALS_DIR__", originals_dir)
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

        // If the local Headroom cache hop is on, the proxy launchd job can take a few seconds
        // to bind. Verify it actually came up and warn loudly if not — the router's
        // compressionProxyURL now points at it, so a dead proxy would break the local path.
        let oc = &cfg.opencode_local;
        if oc.enabled && oc.headroom {
            let port = oc.headroom_port.unwrap_or(8787) as u16;
            let mut up = false;
            for _ in 0..25 {
                if proxy_reachable(port) {
                    up = true;
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(400));
            }
            if up {
                println!("  ✓ headroom proxy reachable on :{port}");
            } else {
                let venv = cfg
                    .global
                    .headroom_venv
                    .as_deref()
                    .unwrap_or("~/.headroom-venv");
                eprintln!(
                    "  ⚠ headroom proxy is NOT up on :{port} after ~10s — the router's \
                     compressionProxyURL points at it, so the local path fails until it runs.\n    \
                     See /tmp/ccstack-headroom-proxy.log. Common cause: the venv lacks the `proxy` \
                     extra → run\n      {venv}/bin/python -m pip install 'headroom-ai[proxy]'\n    \
                     then `launchctl kickstart -k gui/$(id -u)/com.ccstack.headroom-proxy`."
                );
            }
        }

        // The compression daemon pre-loads the Headroom model BEFORE binding its socket, so a
        // present socket means "warm + ready" (can take ~10-15s). Wait for it and report.
        let chk = &cfg.compress_hook;
        if chk.enabled {
            let socket = chk
                .socket
                .clone()
                .unwrap_or_else(|| "~/.config/ccstack/compress-daemon.sock".to_string());
            let sock_path = util::expand_tilde(&socket).unwrap_or_else(|_| socket.clone().into());
            let mut up = false;
            for _ in 0..50 {
                if sock_path.exists() {
                    up = true;
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(400));
            }
            if up {
                println!(
                    "  ✓ compress daemon warm + listening on {}",
                    sock_path.display()
                );
            } else {
                eprintln!(
                    "  ⚠ compress daemon socket not up after ~20s ({}). The PostToolUse hook \
                     safely passes through until it warms.\n    See /tmp/ccstack-compress-daemon.log; \
                     retry `launchctl kickstart -k gui/$(id -u)/com.ccstack.compress-daemon`.",
                    sock_path.display()
                );
            }
        }
    }
    Ok(())
}

/// Quick TCP reachability probe for a local port (the Headroom proxy).
fn proxy_reachable(port: u16) -> bool {
    use std::net::TcpStream;
    use std::time::Duration;
    format!("127.0.0.1:{port}")
        .parse()
        .ok()
        .and_then(|addr| TcpStream::connect_timeout(&addr, Duration::from_millis(400)).ok())
        .is_some()
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
