use anyhow::Result;
use serde_json::{Map, Value};
use std::path::Path;

use crate::state::Prior;
use crate::util;

fn read_json_or_empty(p: &Path) -> Result<Value> {
    if !p.exists() {
        return Ok(Value::Object(Map::new()));
    }
    let txt = std::fs::read_to_string(p)?;
    if txt.trim().is_empty() {
        return Ok(Value::Object(Map::new()));
    }
    Ok(serde_json::from_str(&txt)?)
}

fn write_json(p: &Path, v: &Value) -> Result<()> {
    if let Some(parent) = p.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(p, serde_json::to_string_pretty(v)?)?;
    Ok(())
}

fn get_at<'a>(v: &'a Value, parts: &[&str]) -> Option<&'a Value> {
    let mut cur = v;
    for p in parts {
        cur = cur.get(*p)?;
    }
    Some(cur)
}

fn set_at(v: &mut Value, parts: &[&str], val: Value) {
    if parts.is_empty() {
        return;
    }
    if !v.is_object() {
        *v = Value::Object(Map::new());
    }
    let obj = v.as_object_mut().expect("object");
    if parts.len() == 1 {
        obj.insert(parts[0].to_string(), val);
    } else {
        let child = obj
            .entry(parts[0].to_string())
            .or_insert_with(|| Value::Object(Map::new()));
        set_at(child, &parts[1..], val);
    }
}

fn remove_at(v: &mut Value, parts: &[&str]) {
    if parts.is_empty() || !v.is_object() {
        return;
    }
    let obj = v.as_object_mut().expect("object");
    if parts.len() == 1 {
        obj.remove(parts[0]);
        return;
    }
    if let Some(child) = obj.get_mut(parts[0]) {
        remove_at(child, &parts[1..]);
        // prune now-empty parent objects ccstack may have created
        if child.as_object().map(|m| m.is_empty()).unwrap_or(false) {
            obj.remove(parts[0]);
        }
    }
}

/// Snapshot a file before first touch (coarse-grained restore fallback).
pub fn backup_file(target: &Path) -> Result<Option<String>> {
    if !target.exists() {
        return Ok(None);
    }
    let dir = util::ccstack_dir()?.join("backups").join(util::now_ts());
    std::fs::create_dir_all(&dir)?;
    let name = target.file_name().unwrap_or(std::ffi::OsStr::new("file"));
    let dest = dir.join(name);
    std::fs::copy(target, &dest)?;
    Ok(Some(dest.to_string_lossy().into_owned()))
}

// ---- json_key ----

/// Current on-disk hash of a json key (for drift checks). `None` if absent.
pub fn current_json_key_hash(target: &Path, key_path: &str) -> Result<Option<String>> {
    let root = read_json_or_empty(target)?;
    let parts: Vec<&str> = key_path.split('.').collect();
    Ok(get_at(&root, &parts).map(|v| util::sha256_hex(v.to_string().as_bytes())))
}

pub fn apply_json_key(
    target: &Path,
    key_path: &str,
    value: Value,
    dry: bool,
) -> Result<(Prior, String)> {
    let mut root = read_json_or_empty(target)?;
    let parts: Vec<&str> = key_path.split('.').collect();
    let prior_val = get_at(&root, &parts).cloned();
    let region_hash = util::sha256_hex(value.to_string().as_bytes());
    if dry {
        println!("  ~ {} :: {} = {}", target.display(), key_path, value);
        return Ok((
            Prior {
                present: prior_val.is_some(),
                value: prior_val,
                snapshot: None,
            },
            region_hash,
        ));
    }
    let snapshot = backup_file(target)?;
    set_at(&mut root, &parts, value);
    write_json(target, &root)?;
    Ok((
        Prior {
            present: prior_val.is_some(),
            value: prior_val,
            snapshot,
        },
        region_hash,
    ))
}

pub fn revert_json_key(target: &Path, key_path: &str, prior: &Prior) -> Result<()> {
    let mut root = read_json_or_empty(target)?;
    let parts: Vec<&str> = key_path.split('.').collect();
    match (prior.present, &prior.value) {
        (true, Some(v)) => set_at(&mut root, &parts, v.clone()),
        _ => remove_at(&mut root, &parts),
    }
    write_json(target, &root)?;
    Ok(())
}

// ---- file_create ----

pub fn current_file_hash(target: &Path) -> Result<Option<String>> {
    if !target.exists() {
        return Ok(None);
    }
    Ok(Some(util::sha256_hex(&std::fs::read(target)?)))
}

pub fn apply_file_create(target: &Path, contents: &str, dry: bool) -> Result<(Prior, String)> {
    let existed = target.exists();
    let region_hash = util::sha256_hex(contents.as_bytes());
    if dry {
        println!("  + {} (file_create)", target.display());
        return Ok((
            Prior {
                present: existed,
                value: None,
                snapshot: None,
            },
            region_hash,
        ));
    }
    let snapshot = if existed { backup_file(target)? } else { None };
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(target, contents)?;
    Ok((
        Prior {
            present: existed,
            value: None,
            snapshot,
        },
        region_hash,
    ))
}

pub fn revert_file_create(target: &Path, prior: &Prior) -> Result<()> {
    if prior.present {
        if let Some(snap) = &prior.snapshot {
            std::fs::copy(snap, target)?;
        }
    } else if target.exists() {
        std::fs::remove_file(target)?;
    }
    Ok(())
}

// ---- text_block (sentinel-delimited region inside a text file) ----

fn block_markers(marker: &str) -> (String, String) {
    (
        format!("<!-- >>> ccstack:{} >>> -->", marker),
        format!("<!-- <<< ccstack:{} <<< -->", marker),
    )
}

fn extract_block(text: &str, marker: &str) -> Option<String> {
    let (begin, end) = block_markers(marker);
    let b = text.find(&begin)?;
    let after = b + begin.len();
    let e = text[after..].find(&end)? + after;
    Some(text[after..e].trim_matches('\n').to_string())
}

/// Hash of the current block's inner content (drift check). None if absent.
pub fn current_text_block_hash(target: &Path, marker: &str) -> Result<Option<String>> {
    if !target.exists() {
        return Ok(None);
    }
    let text = std::fs::read_to_string(target)?;
    Ok(extract_block(&text, marker).map(|inner| util::sha256_hex(inner.as_bytes())))
}

pub fn apply_text_block(
    target: &Path,
    marker: &str,
    contents: &str,
    dry: bool,
) -> Result<(Prior, String)> {
    let (begin, end) = block_markers(marker);
    let existed = target.exists();
    let region_hash = util::sha256_hex(contents.as_bytes());
    if dry {
        println!("  ~ {} :: text_block '{}'", target.display(), marker);
        return Ok((
            Prior {
                present: existed,
                value: None,
                snapshot: None,
            },
            region_hash,
        ));
    }
    let snapshot = if existed { backup_file(target)? } else { None };
    let old = if existed {
        std::fs::read_to_string(target)?
    } else {
        String::new()
    };
    let block = format!("{}\n{}\n{}", begin, contents, end);
    let new = match (old.find(&begin), old.find(&end)) {
        (Some(b), Some(_)) => {
            let after = b + begin.len();
            let e = old[after..].find(&end).unwrap() + after + end.len();
            format!("{}{}{}", &old[..b], block, &old[e..])
        }
        _ if old.trim().is_empty() => format!("{}\n", block),
        _ => format!("{}\n\n{}\n", old.trim_end(), block),
    };
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(target, new)?;
    Ok((
        Prior {
            present: existed,
            value: None,
            snapshot,
        },
        region_hash,
    ))
}

pub fn revert_text_block(target: &Path, marker: &str, prior: &Prior) -> Result<()> {
    if !target.exists() {
        return Ok(());
    }
    let (begin, end) = block_markers(marker);
    let old = std::fs::read_to_string(target)?;
    let new = match (old.find(&begin), old.find(&end)) {
        (Some(b), Some(_)) => {
            let after = b + begin.len();
            let e = old[after..].find(&end).unwrap() + after + end.len();
            let mut s = old[..b].trim_end().to_string();
            s.push_str(old[e..].trim_end());
            s
        }
        _ => old.clone(),
    };
    if new.trim().is_empty() && !prior.present {
        std::fs::remove_file(target)?;
    } else {
        let mut out = new.trim_end().to_string();
        if !out.is_empty() {
            out.push('\n');
        }
        std::fs::write(target, out)?;
    }
    Ok(())
}

// ---- service (run a start command; reverse by stopping) ----

fn run_shell(cmd: &str) -> Result<bool> {
    let status = std::process::Command::new("sh")
        .arg("-c")
        .arg(cmd)
        .status()?;
    Ok(status.success())
}

pub fn apply_service(start_cmd: &str, dry: bool) -> Result<(Prior, String)> {
    let region_hash = util::sha256_hex(start_cmd.as_bytes());
    if dry {
        println!("  ▶ service start: {}", start_cmd);
        return Ok((
            Prior {
                present: false,
                value: None,
                snapshot: None,
            },
            region_hash,
        ));
    }
    if !run_shell(start_cmd)? {
        anyhow::bail!("service start failed: {}", start_cmd);
    }
    Ok((
        Prior {
            present: false,
            value: None,
            snapshot: None,
        },
        region_hash,
    ))
}

pub fn revert_service(stop_cmd: &str) -> Result<()> {
    let _ = run_shell(stop_cmd)?; // best-effort stop
    Ok(())
}

// ---- pkg_install (create a Python venv + pip install; opt-in removal) ----

fn which(cmd: &str) -> Option<String> {
    let out = std::process::Command::new("sh")
        .arg("-c")
        .arg(format!("command -v {}", cmd))
        .output()
        .ok()?;
    if out.status.success() {
        let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if !s.is_empty() {
            return Some(s);
        }
    }
    None
}

/// Find a Python <=3.13 interpreter (headroom-ai's Rust/PyO3 core caps at 3.13).
fn find_python313() -> Option<String> {
    for c in [
        "/opt/homebrew/opt/python@3.13/bin/python3.13",
        "/usr/local/opt/python@3.13/bin/python3.13",
        "/usr/bin/python3.13",
    ] {
        if Path::new(c).exists() {
            return Some(c.to_string());
        }
    }
    which("python3.13")
}

/// "present" if the venv already has a headroom binary, else None.
pub fn current_pkg_install(venv: &Path) -> Result<Option<String>> {
    Ok(if venv.join("bin/headroom").exists() {
        Some("present".to_string())
    } else {
        None
    })
}

/// Create a 3.13 venv and `pip install <spec>` if it doesn't exist yet.
/// No-ops (and records `present`) if the venv is already there, so revert never
/// removes a venv ccstack didn't create.
pub fn apply_pkg_install(venv: &Path, spec: &str, dry: bool) -> Result<(Prior, String)> {
    let region_hash = util::sha256_hex(spec.as_bytes());
    let existed = venv.join("bin/headroom").exists();
    if dry {
        println!("  ⬇ pkg_install: {} → {}", spec, venv.display());
        return Ok((
            Prior {
                present: existed,
                value: None,
                snapshot: None,
            },
            region_hash,
        ));
    }
    if existed {
        return Ok((
            Prior {
                present: true,
                value: None,
                snapshot: None,
            },
            region_hash,
        ));
    }
    let py = find_python313().ok_or_else(|| {
        anyhow::anyhow!("Python 3.13 not found — headroom-ai needs <=3.13. Install it: brew install python@3.13")
    })?;
    println!(
        "  installing {} into {} (this can take a few minutes)…",
        spec,
        venv.display()
    );
    if !run_shell(&format!("'{}' -m venv '{}'", py, venv.display()))? {
        anyhow::bail!("failed to create venv at {}", venv.display());
    }
    let pip = venv.join("bin/pip");
    let _ = run_shell(&format!(
        "'{}' install -q --upgrade pip",
        pip.to_string_lossy()
    ));
    if !run_shell(&format!("'{}' install '{}'", pip.to_string_lossy(), spec))? {
        anyhow::bail!("pip install '{}' failed", spec);
    }
    Ok((
        Prior {
            present: false,
            value: None,
            snapshot: None,
        },
        region_hash,
    ))
}

pub fn revert_pkg_install(venv: &Path, prior: &Prior) -> Result<()> {
    // opt-in removal: only delete a venv ccstack created, never a pre-existing one.
    if !prior.present && venv.exists() {
        std::fs::remove_dir_all(venv)?;
    }
    Ok(())
}
