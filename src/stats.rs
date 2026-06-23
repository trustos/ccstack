use anyhow::Result;
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::util;

#[derive(Default, Clone)]
struct Usage {
    input: u64,
    output: u64,
    cache_read: u64,
    cache_create: u64,
}

impl Usage {
    fn add(&mut self, o: &Usage) {
        self.input += o.input;
        self.output += o.output;
        self.cache_read += o.cache_read;
        self.cache_create += o.cache_create;
    }
    fn prompt_total(&self) -> u64 {
        self.input + self.cache_read + self.cache_create
    }
    fn hit_pct(&self) -> f64 {
        let p = self.prompt_total();
        if p == 0 {
            0.0
        } else {
            self.cache_read as f64 / p as f64 * 100.0
        }
    }
}

/// Approximate Anthropic prices, USD per **million** tokens:
/// (input, output, cache_read, cache_write). EDIT to current pricing.
/// Non-Claude (local) models return None => $0.
fn price(model: &str) -> Option<(f64, f64, f64, f64)> {
    let m = model.to_lowercase();
    if m.contains("opus") {
        Some((15.0, 75.0, 1.5, 18.75))
    } else if m.contains("sonnet") {
        Some((3.0, 15.0, 0.3, 3.75))
    } else if m.contains("haiku") {
        Some((1.0, 5.0, 0.1, 1.25))
    } else {
        None
    }
}

fn cost_of(model: &str, u: &Usage) -> f64 {
    match price(model) {
        Some((pi, po, pcr, pcw)) => {
            (u.input as f64 * pi
                + u.output as f64 * po
                + u.cache_read as f64 * pcr
                + u.cache_create as f64 * pcw)
                / 1_000_000.0
        }
        None => 0.0,
    }
}

struct SessionStat {
    id: String,
    model: String,
    usage: Usage,
    cost: f64,
}

fn projects_dir() -> Result<PathBuf> {
    Ok(util::home()?.join(".claude").join("projects"))
}

fn find_jsonl(dir: &Path, out: &mut Vec<PathBuf>) -> Result<()> {
    if !dir.exists() {
        return Ok(());
    }
    for entry in std::fs::read_dir(dir)? {
        let p = entry?.path();
        if p.is_dir() {
            find_jsonl(&p, out)?;
        } else if p.extension().and_then(|x| x.to_str()) == Some("jsonl") {
            out.push(p);
        }
    }
    Ok(())
}

fn parse_line_usage(v: &Value) -> Option<(String, Usage)> {
    let usage = v.pointer("/message/usage")?;
    let g = |k: &str| usage.get(k).and_then(|x| x.as_u64()).unwrap_or(0);
    let model = v
        .pointer("/message/model")
        .and_then(|x| x.as_str())
        .unwrap_or("unknown")
        .to_string();
    let u = Usage {
        input: g("input_tokens"),
        output: g("output_tokens"),
        cache_read: g("cache_read_input_tokens"),
        cache_create: g("cache_creation_input_tokens"),
    };
    if u.input + u.output + u.cache_read + u.cache_create == 0 {
        return None;
    }
    Some((model, u))
}

fn parse_session(path: &Path) -> Result<SessionStat> {
    let txt = std::fs::read_to_string(path)?;
    let mut usage = Usage::default();
    let mut cost = 0.0;
    let mut model = String::from("-");
    for line in txt.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let v: Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => continue,
        };
        if let Some((m, u)) = parse_line_usage(&v) {
            cost += cost_of(&m, &u);
            usage.add(&u);
            // remember a cloud model name if any turn used one (or fill the "-" placeholder)
            if price(&m).is_some() || model == "-" {
                model = m;
            }
        }
    }
    let id = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "session".into());
    Ok(SessionStat {
        id,
        model,
        usage,
        cost,
    })
}

fn headroom_stats_json() -> Option<Value> {
    let out = Command::new("curl")
        .args(["-s", "--max-time", "2", "http://localhost:8787/stats"])
        .output()
        .ok()?;
    if !out.status.success() || out.stdout.is_empty() {
        return None;
    }
    serde_json::from_slice(&out.stdout).ok()
}

fn trunc(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        return s.to_string();
    }
    let t: String = s.chars().take(n.saturating_sub(1)).collect();
    format!("{}…", t)
}

fn short_model(m: &str) -> &str {
    if m.contains("opus") {
        "claude-opus"
    } else if m.contains("sonnet") {
        "claude-sonnet"
    } else if m.contains("haiku") {
        "claude-haiku"
    } else {
        m
    }
}

fn collect_sessions(filter: Option<&str>) -> Vec<SessionStat> {
    let mut files = Vec::new();
    if let Ok(dir) = projects_dir() {
        let _ = find_jsonl(&dir, &mut files);
    }
    let mut sessions: Vec<SessionStat> = Vec::new();
    for f in &files {
        if let Ok(s) = parse_session(f) {
            if s.usage.prompt_total() + s.usage.output > 0 {
                sessions.push(s);
            }
        }
    }
    if let Some(sid) = filter {
        sessions.retain(|s| s.id.contains(sid));
    }
    sessions.sort_by(|a, b| {
        b.cost
            .partial_cmp(&a.cost)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    sessions
}

pub fn run(json: bool, session: Option<String>) -> Result<()> {
    let sessions = collect_sessions(session.as_deref());

    let mut total = Usage::default();
    let mut total_cost = 0.0;
    for s in &sessions {
        total.add(&s.usage);
        total_cost += s.cost;
    }

    let hr = headroom_stats_json();

    if json {
        let sess_json: Vec<Value> = sessions
            .iter()
            .map(|s| {
                serde_json::json!({
                    "id": s.id, "model": s.model,
                    "input": s.usage.input, "output": s.usage.output,
                    "cache_read": s.usage.cache_read, "cache_create": s.usage.cache_create,
                    "cache_hit_pct": s.usage.hit_pct(), "cost_usd": s.cost,
                })
            })
            .collect();
        let report = serde_json::json!({
            "sessions": sess_json,
            "total": {
                "sessions": sessions.len(),
                "input": total.input, "output": total.output,
                "cache_read": total.cache_read, "cache_create": total.cache_create,
                "cache_hit_pct": total.hit_pct(), "cost_usd": total_cost,
            },
            "compression": hr,
        });
        println!("{}", serde_json::to_string_pretty(&report)?);
        return Ok(());
    }

    if sessions.is_empty() {
        println!(
            "No Claude Code sessions found under {}.",
            projects_dir()?.display()
        );
    } else {
        println!(
            "{:<22} {:<14} {:>10} {:>9} {:>11} {:>7} {:>9}",
            "session", "model", "input", "output", "cache_read", "hit%", "$"
        );
        for s in &sessions {
            println!(
                "{:<22} {:<14} {:>10} {:>9} {:>11} {:>7.1} {:>9.3}",
                trunc(&s.id, 22),
                trunc(short_model(&s.model), 14),
                s.usage.input,
                s.usage.output,
                s.usage.cache_read,
                s.usage.hit_pct(),
                s.cost
            );
        }
        println!("{:-<86}", "");
        println!(
            "{:<22} {:<14} {:>10} {:>9} {:>11} {:>7.1} {:>9.3}",
            format!("TOTAL ({} sess)", sessions.len()),
            "",
            total.input,
            total.output,
            total.cache_read,
            total.hit_pct(),
            total_cost
        );
        println!("  cache_write(create) total: {}", total.cache_create);
    }

    println!();
    match hr {
        Some(v) => {
            let saved = v.pointer("/tokens/saved").and_then(|x| x.as_u64());
            let pct = v
                .pointer("/tokens/savings_percent")
                .or_else(|| v.pointer("/tokens/savingsPercent"))
                .and_then(|x| x.as_f64());
            let cost_saved = v.pointer("/cost/savings").and_then(|x| x.as_f64());
            println!("Headroom (compression, cumulative since proxy start):");
            println!(
                "  tokens saved: {}   savings: {}   est $ saved: {}",
                saved.map(|x| x.to_string()).unwrap_or_else(|| "?".into()),
                pct.map(|x| format!("{:.1}%", x))
                    .unwrap_or_else(|| "?".into()),
                cost_saved
                    .map(|x| format!("${:.2}", x))
                    .unwrap_or_else(|| "?".into())
            );
        }
        None => {
            println!("Headroom: proxy not reachable on :8787 — start it (`headroom wrap claude` / `headroom proxy`),");
            println!("  or run `headroom stats` directly for cumulative compression numbers.");
        }
    }

    println!();
    println!("notes:");
    println!("  • $ uses an approximate built-in price table (edit PRICES in stats.rs); local-model tokens are $0.");
    println!("  • a big cache_read column = prompt caching is working. true cost ≈ input + output + small cache_read.");
    println!(
        "  • cross-check Anthropic accounting with `bunx ccusage` (maintained pricing + dedup)."
    );
    println!("  • local-model (oMLX) cache truth is in oMLX `usage.cached_tokens` (see agentic_eval) if the router");
    println!("    doesn't surface it as cache_read here.");
    Ok(())
}

/// Cumulative totals across all sessions + Headroom compression — shared with `measure`.
#[derive(Default, serde::Serialize, serde::Deserialize)]
pub struct Agg {
    pub sessions: usize,
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_create: u64,
    pub cost_usd: f64,
    pub hr_saved: u64,
    pub hr_input: u64,
    pub hr_output: u64,
}

pub fn aggregate() -> Result<Agg> {
    let sessions = collect_sessions(None);
    let mut a = Agg {
        sessions: sessions.len(),
        ..Default::default()
    };
    for s in &sessions {
        a.input += s.usage.input;
        a.output += s.usage.output;
        a.cache_read += s.usage.cache_read;
        a.cache_create += s.usage.cache_create;
        a.cost_usd += s.cost;
    }
    if let Some(v) = headroom_stats_json() {
        a.hr_saved = v
            .pointer("/tokens/saved")
            .and_then(|x| x.as_u64())
            .unwrap_or(0);
        a.hr_input = v
            .pointer("/tokens/input")
            .and_then(|x| x.as_u64())
            .unwrap_or(0);
        a.hr_output = v
            .pointer("/tokens/output")
            .and_then(|x| x.as_u64())
            .unwrap_or(0);
    }
    Ok(a)
}
