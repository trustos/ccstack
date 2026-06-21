mod config;
mod measure;
mod ops;
mod render;
mod state;
mod stats;
mod util;

use anyhow::Result;
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(
    name = "ccstack",
    version,
    about = "Reversible setup & config manager for Claude Code (cloud + hybrid local + Headroom)."
)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Write a sample ccstack.toml to ~/.config/ccstack/.
    Init,
    /// Preview the change set without writing (apply --dry-run).
    Plan,
    /// Render the declared config into managed files; records every change.
    Apply {
        #[arg(long)]
        dry_run: bool,
    },
    /// Show every applied change and whether the live file still matches.
    Status,
    /// Verify ledger integrity vs disk (nonzero exit on drift).
    Verify,
    /// Revert changes: --all, --profile <p>, or --change <id>.
    Revert {
        #[arg(long)]
        all: bool,
        #[arg(long)]
        profile: Option<String>,
        #[arg(long)]
        change: Option<String>,
    },
    /// Clean slate: revert everything ccstack applied.
    Uninstall,
    /// Per-session and total token/cache/$ usage + Headroom compression.
    Stats {
        /// Machine-readable JSON output.
        #[arg(long)]
        json: bool,
        /// Filter to sessions whose id contains this substring.
        #[arg(long)]
        session: Option<String>,
    },
    /// A/B measure the stack around a task (begin / end / compare).
    Measure {
        #[command(subcommand)]
        action: MeasureCmd,
    },
}

#[derive(Subcommand)]
enum MeasureCmd {
    /// Snapshot metrics right before a task.
    Begin { label: String },
    /// Compute the task's deltas since `begin`.
    End { label: String },
    /// Show two runs side by side (e.g. off vs on).
    Compare { a: String, b: String },
}

fn main() -> Result<()> {
    match Cli::parse().cmd {
        Cmd::Init => ops::init(),
        Cmd::Plan => ops::apply(true),
        Cmd::Apply { dry_run } => ops::apply(dry_run),
        Cmd::Status => ops::status(),
        Cmd::Verify => ops::verify(),
        Cmd::Revert { all, profile, change } => ops::revert(all, profile, change),
        Cmd::Uninstall => ops::revert(true, None, None),
        Cmd::Stats { json, session } => stats::run(json, session),
        Cmd::Measure { action } => match action {
            MeasureCmd::Begin { label } => measure::begin(&label),
            MeasureCmd::End { label } => measure::end(&label),
            MeasureCmd::Compare { a, b } => measure::compare(&a, &b),
        },
    }
}
