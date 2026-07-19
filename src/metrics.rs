//! Engineering + product KPIs for a run. Computed from the final records and the
//! run counters, printed as a summary and returned as JSON for dashboards.

use crate::config::Role;
use crate::schema::{CandidateRecord, RowStatus};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::time::Duration;

/// Raw counters gathered during a run.
pub struct RunStats {
    pub total_files: usize,
    pub cached: usize,
    pub extracted_ok: usize,
    pub failures: usize,
    pub timeouts: usize,
    pub elapsed: Duration,
    pub calls: u64,
    pub in_tokens: u64,
    pub out_tokens: u64,
}

fn pct(n: usize, d: usize) -> f64 {
    if d == 0 { 0.0 } else { n as f64 * 100.0 / d as f64 }
}

/// flash-lite list price (approx): $0.10/M in, $0.40/M out (adjust per model).
fn cost(in_tok: u64, out_tok: u64) -> f64 {
    in_tok as f64 / 1e6 * 0.10 + out_tok as f64 / 1e6 * 0.40
}

/// Compute KPIs, print a human summary, and return the JSON for `metrics.json`.
pub fn report(stats: &RunStats, records: &[CandidateRecord]) -> Value {
    let attempted = stats.extracted_ok + stats.failures;
    let mins = stats.elapsed.as_secs_f64() / 60.0;
    let total_tokens = stats.in_tokens + stats.out_tokens;
    let run_cost = cost(stats.in_tokens, stats.out_tokens);

    // ---- product side (over the final deduped record set) ----
    let n = records.len();
    let verified = records.iter().filter(|r| r.status == RowStatus::AutoVerified).count();
    let with_email = records.iter().filter(|r| !r.email.is_empty()).count();
    let with_phone = records.iter().filter(|r| !r.whatsapp.is_empty()).count();
    let with_both = records.iter().filter(|r| !r.email.is_empty() && !r.whatsapp.is_empty()).count();
    let with_name = records.iter().filter(|r| !r.name.is_empty()).count();
    let with_role = records.iter().filter(|r| r.primary_role != Role::Other).count();
    let employed = records.iter().filter(|r| r.currently_employed).count();
    let gaps = records.iter().filter(|r| r.has_gap).count();
    let freshers = records.iter().filter(|r| {
        matches!(r.experience_level, crate::config::ExperienceLevel::Fresher)
    }).count();
    let vision = records.iter().filter(|r| r.used_vision).count();

    let mut conf: BTreeMap<&str, usize> = BTreeMap::new();
    for r in records {
        *conf.entry(r.overall_confidence.label()).or_insert(0) += 1;
    }
    let mut role_dist: BTreeMap<&str, usize> = BTreeMap::new();
    for r in records {
        *role_dist.entry(r.primary_role.label()).or_insert(0) += 1;
    }
    let mut roles_sorted: Vec<(&&str, &usize)> = role_dist.iter().collect();
    roles_sorted.sort_by(|a, b| b.1.cmp(a.1));

    // ---- print ----
    println!("\n╭─ Engineering KPIs ─────────────────────────");
    println!("│ Files discovered      {}", stats.total_files);
    println!("│ Cache hit rate        {:.0}%  ({} cached, {} extracted)", pct(stats.cached, stats.total_files), stats.cached, attempted);
    println!("│ Parse success rate    {:.1}%  ({}/{} attempted)", pct(stats.extracted_ok, attempted.max(1)), stats.extracted_ok, attempted);
    println!("│ Failures / timeouts   {} / {}", stats.failures, stats.timeouts);
    if attempted > 0 && mins > 0.0 {
        println!("│ Throughput            {:.1} resumes/min", attempted as f64 / mins);
        println!("│ Avg latency/resume    {:.1}s", stats.elapsed.as_secs_f64() / attempted as f64);
    }
    println!("│ Wall-clock            {:.1}s", stats.elapsed.as_secs_f64());
    println!("│ Gemini API calls      {}  ({:.1}/resume)", stats.calls, stats.calls as f64 / attempted.max(1) as f64);
    println!("│ Tokens                {} in + {} out = {}", stats.in_tokens, stats.out_tokens, total_tokens);
    println!("│ Cost                  ${:.4}  (${:.5}/resume)", run_cost, run_cost / attempted.max(1) as f64);
    println!("│ Vision fallbacks      {}", vision);
    println!("╰────────────────────────────────────────────");

    println!("╭─ Product KPIs ─────────────────────────────");
    println!("│ Candidates            {}", n);
    println!("│ Auto-verified         {:.0}%  ({}/{})   Review {:.0}%  ({})", pct(verified, n), verified, n, pct(n - verified, n), n - verified);
    println!("│ Field coverage        email {:.0}% · phone {:.0}% · both {:.0}% · name {:.0}% · role {:.0}%",
        pct(with_email, n), pct(with_phone, n), pct(with_both, n), pct(with_name, n), pct(with_role, n));
    println!("│ Confidence            {}", conf.iter().map(|(k, v)| format!("{k}:{v}")).collect::<Vec<_>>().join("  "));
    println!("│ Experience mix        {} experienced · {} fresher", n - freshers, freshers);
    println!("│ Employment            {} employed · {} not · {} with current gap", employed, n - employed, gaps);
    println!("│ Top roles             {}", roles_sorted.iter().take(5).map(|(k, v)| format!("{k}:{v}")).collect::<Vec<_>>().join("  "));
    println!("╰────────────────────────────────────────────");

    json!({
        "engineering": {
            "files_discovered": stats.total_files,
            "cached": stats.cached,
            "extracted_attempted": attempted,
            "extracted_ok": stats.extracted_ok,
            "failures": stats.failures,
            "timeouts": stats.timeouts,
            "cache_hit_rate_pct": pct(stats.cached, stats.total_files),
            "parse_success_rate_pct": pct(stats.extracted_ok, attempted.max(1)),
            "wall_clock_secs": stats.elapsed.as_secs_f64(),
            "throughput_per_min": if mins > 0.0 { attempted as f64 / mins } else { 0.0 },
            "avg_latency_secs": if attempted > 0 { stats.elapsed.as_secs_f64() / attempted as f64 } else { 0.0 },
            "gemini_calls": stats.calls,
            "calls_per_resume": stats.calls as f64 / attempted.max(1) as f64,
            "tokens_in": stats.in_tokens,
            "tokens_out": stats.out_tokens,
            "cost_usd": run_cost,
            "cost_per_resume_usd": run_cost / attempted.max(1) as f64,
            "vision_fallbacks": vision,
        },
        "product": {
            "candidates": n,
            "auto_verified": verified,
            "auto_verify_rate_pct": pct(verified, n),
            "review_rate_pct": pct(n - verified, n),
            "coverage": {
                "email_pct": pct(with_email, n),
                "phone_pct": pct(with_phone, n),
                "both_pct": pct(with_both, n),
                "name_pct": pct(with_name, n),
                "role_pct": pct(with_role, n),
            },
            "confidence": conf,
            "experienced": n - freshers,
            "freshers": freshers,
            "employed": employed,
            "with_current_gap": gaps,
            "role_distribution": role_dist,
        }
    })
}
