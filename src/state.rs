use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

use crate::util;

/// The kind of change ccstack made — determines how it is reverted.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChangeKind {
    JsonKey,
    TextBlock,
    FileCreate,
    PkgInstall,
    Service,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Applied,
    Reverted,
}

/// What existed before ccstack touched the target — enough to restore it exactly.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Prior {
    pub present: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<serde_json::Value>,
    /// Full-file snapshot path taken before first touch (coarse fallback).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snapshot: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Change {
    pub id: String,
    pub txn: String,
    pub profile: String,
    pub kind: ChangeKind,
    pub target: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key_path: Option<String>,
    pub prior: Prior,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub applied_value: Option<String>,
    pub region_hash: String,
    pub status: Status,
}

/// The ledger — ccstack's authoritative record of everything it applied.
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Ledger {
    #[serde(default)]
    pub changes: Vec<Change>,
}

impl Ledger {
    pub fn path() -> Result<PathBuf> {
        Ok(util::ccstack_dir()?.join("state.json"))
    }

    pub fn load() -> Result<Self> {
        let p = Self::path()?;
        if !p.exists() {
            return Ok(Self::default());
        }
        let txt = std::fs::read_to_string(&p)?;
        Ok(serde_json::from_str(&txt).unwrap_or_default())
    }

    pub fn save(&self) -> Result<()> {
        let dir = util::ccstack_dir()?;
        std::fs::create_dir_all(&dir)?;
        std::fs::write(Self::path()?, serde_json::to_string_pretty(self)?)?;
        Ok(())
    }

    pub fn next_id(&self) -> String {
        format!("chg_{:04}", self.changes.len() + 1)
    }

    /// Iterator over currently-applied (not reverted) changes.
    pub fn active(&self) -> impl Iterator<Item = &Change> {
        self.changes.iter().filter(|c| c.status == Status::Applied)
    }
}
