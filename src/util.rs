use anyhow::{Context, Result};
use sha2::{Digest, Sha256};
use std::path::PathBuf;

pub fn home() -> Result<PathBuf> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .context("HOME env var not set")
}

/// Expand a leading `~/` to the user's home directory.
pub fn expand_tilde(p: &str) -> Result<PathBuf> {
    match p.strip_prefix("~/") {
        Some(rest) => Ok(home()?.join(rest)),
        None => Ok(PathBuf::from(p)),
    }
}

pub fn ccstack_dir() -> Result<PathBuf> {
    Ok(home()?.join(".config").join("ccstack"))
}

pub fn sha256_hex(data: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(data);
    format!("{:x}", h.finalize())
}

/// Unix-epoch seconds as a string (used for txn ids and backup dirs).
pub fn now_ts() -> String {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
        .to_string()
}
