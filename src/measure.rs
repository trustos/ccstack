use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

use crate::stats::{self, Agg};
use crate::util;

fn measure_dir() -> Result<PathBuf> {
    let d = util::ccstack_dir()?.join("measure");
    std::fs::create_dir_all(&d)?;
    Ok(d)
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[derive(Serialize, Deserialize)]
struct Snapshot {
    ts: u64,
    agg: Agg,
}

#[derive(Serialize, Deserialize)]
struct RunDelta {
    label: String,
    ts_start: u64,
    ts_end: u64,
    input: u64,
    output: u64,
    cache_read: u64,
    cache_create: u64,
    cost_usd: f64,
    hr_saved: u64,
    sessions_delta: i64,
}

/// Snapshot cumulative metrics right before a task.
pub fn begin(label: &str) -> Result<()> {
    let snap = Snapshot {
        ts: now_secs(),
        agg: stats::aggregate()?,
    };
    let p = measure_dir()?.join(format!("{label}.begin.json"));
    std::fs::write(&p, serde_json::to_string_pretty(&snap)?)?;
    println!("measure '{label}' started — baseline snapshot saved.");
    println!("run your task now, then: ccstack measure end {label}");
    Ok(())
}

/// Compute the task's deltas since `begin` and store the run.
pub fn end(label: &str) -> Result<()> {
    let bp = measure_dir()?.join(format!("{label}.begin.json"));
    let txt = std::fs::read_to_string(&bp).with_context(|| {
        format!("no baseline for '{label}' — run `ccstack measure begin {label}` first")
    })?;
    let start: Snapshot = serde_json::from_str(&txt)?;
    let now = stats::aggregate()?;

    let d = RunDelta {
        label: label.to_string(),
        ts_start: start.ts,
        ts_end: now_secs(),
        input: now.input.saturating_sub(start.agg.input),
        output: now.output.saturating_sub(start.agg.output),
        cache_read: now.cache_read.saturating_sub(start.agg.cache_read),
        cache_create: now.cache_create.saturating_sub(start.agg.cache_create),
        cost_usd: (now.cost_usd - start.agg.cost_usd).max(0.0),
        hr_saved: now.hr_saved.saturating_sub(start.agg.hr_saved),
        sessions_delta: now.sessions as i64 - start.agg.sessions as i64,
    };

    let rp = measure_dir()?.join(format!("{label}.run.json"));
    std::fs::write(&rp, serde_json::to_string_pretty(&d)?)?;

    let dur = d.ts_end.saturating_sub(d.ts_start);
    println!(
        "run '{}' ({}s, {} new session(s)): input {}  output {}  cache_read {}  $ {:.3}  headroom_saved {}",
        d.label, dur, d.sessions_delta, d.input, d.output, d.cache_read, d.cost_usd, d.hr_saved
    );
    println!("saved -> {}", rp.display());
    println!("A/B two runs with: ccstack measure compare <A> <B>");
    Ok(())
}

fn load_run(label: &str) -> Result<RunDelta> {
    let p = measure_dir()?.join(format!("{label}.run.json"));
    let txt = std::fs::read_to_string(&p)
        .with_context(|| format!("no run '{label}' — run `ccstack measure end {label}` first"))?;
    Ok(serde_json::from_str(&txt)?)
}

fn row_int(name: &str, a: u64, b: u64) {
    println!(
        "{:<16} {:>14} {:>14} {:>12}",
        name,
        a,
        b,
        b as i64 - a as i64
    );
}

fn row_money(name: &str, a: f64, b: f64) {
    println!("{:<16} {:>14.3} {:>14.3} {:>12.3}", name, a, b, b - a);
}

/// Show two runs side by side (e.g. stack off vs on).
pub fn compare(a: &str, b: &str) -> Result<()> {
    let da = load_run(a)?;
    let db = load_run(b)?;
    println!("{:<16} {:>14} {:>14} {:>12}", "metric", a, b, "delta(B-A)");
    row_int("input", da.input, db.input);
    row_int("output", da.output, db.output);
    row_int("cache_read", da.cache_read, db.cache_read);
    row_int("cache_create", da.cache_create, db.cache_create);
    row_money("cost_usd", da.cost_usd, db.cost_usd);
    row_int("hr_saved", da.hr_saved, db.hr_saved);
    println!();
    println!(
        "lower input/cost on the stack-on run = the stack helps; higher cache_read/hr_saved = why."
    );
    Ok(())
}
