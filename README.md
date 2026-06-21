# ccstack

One reversible CLI to set up and manage the **Claude Code** stack — attribution-header KV-cache stability, a hybrid **cloud-plan / local-execute** router, and per-session **token / cache / $ / compression** metrics — where **every change is recorded and fully reversible to a clean slate**.

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

**Status: v0.1** — `json_key` + `file_create` end-to-end (attribution header, executor agent, router config). Next: Headroom `service` lifecycle, a `CLAUDE.md` `text_block` renderer, and a profile-activation selector. Not yet built in CI — compile on macOS and report issues.
