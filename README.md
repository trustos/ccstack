# ccstack

[![ci](https://github.com/trustos/ccstack/actions/workflows/ci.yml/badge.svg)](https://github.com/trustos/ccstack/actions/workflows/ci.yml)

One reversible CLI for the **caching/compression** layer of the stack: the **Claude Code** side (attribution-header KV-cache stability, Headroom MCP compression) and the local **OpenCode → MTPLX Router → mtplx** side (an optional Headroom cache hop inserted between the router and mtplx), plus per-session **token / cache / $ / compression** metrics — where **every change is recorded and fully reversible to a clean slate**.

> `opencode.json` itself (the `mtplx` provider, agents, model wiring) is owned by the **MTPLX Router**, which holds the model list natively and writes the canonical mtplx config. ccstack does **not** touch `opencode.json` — it only inserts the cache hop via the router's `compressionProxyURL`.

Design & rationale: [`../LocalInference/CCSTACK_DESIGN.md`](../LocalInference/CCSTACK_DESIGN.md). End-to-end runbook: [`../LocalInference/CLAUDE_CODE_HANDOFF.md`](../LocalInference/CLAUDE_CODE_HANDOFF.md).

## Build

```bash
make release                 # builds into ./build (gitignored); binary at build/release/ccstack
make install                 # then `ccstack` is on PATH (~/.local/bin)
make help                    # list all targets
# the Makefile sets CARGO_TARGET_DIR=build; raw `cargo` users can export it too
# `brew install rust` if you don't have cargo
```

## What `apply` manages (all reversible)

| Target | Kind | Purpose |
|---|---|---|
| `~/.claude/settings.json` → `env.CLAUDE_CODE_ATTRIBUTION_HEADER=0` | `json_key` | stop the changing billing-header hash from busting KV caches |
| `~/.claude/mcp.json` → `mcpServers.headroom` | `json_key` | register Headroom MCP compression tools (subscription-safe; no proxy) |
| `~/.claude/agents/executor.md` | `file_create` | local-model executor subagent (hybrid) |
| `~/.claude-code-router/config.json` | `file_create` | hybrid router — **API-key billing only** (breaks a Claude subscription's OAuth); opt in with `api_key_billing = true` |
| `~/.claude/CLAUDE.md` → `headroom_compress` rule | `text_block` | sentinel-delimited usage rule (subscription MCP is on-demand) |
| MTPLX Router `config.json` → `compressionProxyURL` | `json_key` | route the router through the Headroom cache hop (when `[opencode_local].headroom = true`) |
| Headroom proxy launchd agent | `service` | run `headroom proxy` (cache mode) → mtplx, when enabled |
| `~/.headroom-venv` (Python 3.13) | `pkg_install` | auto-create venv + `pip install headroom-ai[…]` (opt-in removal; needs `brew install python@3.13`) |

## Commands

```
ccstack init                     # write ~/.config/ccstack/ccstack.toml
ccstack plan                     # preview the change set
ccstack apply [--dry-run]        # render config; record every change
ccstack status                   # applied changes + live sync (in-sync|drifted|missing)
ccstack verify                   # ledger vs disk (nonzero exit on drift)
ccstack revert --all | --profile <p> | --change <id>
ccstack uninstall                # clean slate
ccstack stats [--json] [--session <id>]    # per-session + total tokens/cache/$ + compression
ccstack measure begin|end <label>          # A/B a task
ccstack measure compare <A> <B>            # off vs on, side by side
```

## Reversibility

Every change is logged to `~/.config/ccstack/state.json` with the prior value, a file snapshot, and a content hash. `revert`/`uninstall` restore exactly the prior state and **refuse to touch anything you've since edited** (drift is reported, not clobbered). Full-file backups live under `~/.config/ccstack/backups/`.

**Status:** all five change-kinds implemented end-to-end — `json_key`, `file_create`, `text_block`, `service`, `pkg_install` — covering both the Claude Code side and the local OpenCode→MTPLX stack, including **self-bootstrapping the 3.13 Headroom venv** (`pkg_install`) so there's no manual prerequisite beyond `brew install python@3.13`. Builds in CI on macOS. Next: a profile-activation selector and `stats`/`measure` polish.
