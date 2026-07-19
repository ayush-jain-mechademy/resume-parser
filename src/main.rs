//! CLI entry point: scan a folder of resumes, extract each into a trusted
//! record (using the SQLite cache to skip unchanged files), and write the
//! Excel/CSV/JSON outputs.

use anyhow::{Context, Result};
use clap::Parser;
use futures::stream::{self, StreamExt};
use indicatif::{ProgressBar, ProgressStyle};
use resume_parser::config::Settings;
use resume_parser::gemini::GeminiClient;
use resume_parser::metrics::{self, RunStats};
use resume_parser::schema::{CandidateRecord, RowStatus};
use resume_parser::store::Store;
use resume_parser::{pipeline, util};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::atomic::Ordering;

#[derive(Parser, Debug)]
#[command(name = "resume-parser", about = "Multi-agent resume → trusted Excel extractor")]
struct Cli {
    /// Folder containing resume files (pdf/docx/doc/rtf/txt), searched recursively.
    folder: PathBuf,

    /// Output Excel path (.csv and .json siblings are written too).
    #[arg(short, long, default_value = "candidates.xlsx")]
    output: PathBuf,

    /// SQLite store path (cache + dedupe + audit). Defaults next to the output.
    #[arg(long)]
    store: Option<PathBuf>,

    /// Override the Gemini model for the specialist agents.
    #[arg(long)]
    model: Option<String>,

    /// Max resumes processed concurrently.
    #[arg(long, default_value_t = 6)]
    workers: usize,

    /// Global cap on simultaneous Gemini API requests. Lower if you hit rate limits.
    #[arg(long, default_value_t = 5)]
    max_concurrent: usize,

    /// Hard per-resume time budget in seconds; a file that exceeds it is
    /// abandoned as a failure so it can never stall the batch.
    #[arg(long, default_value_t = 150)]
    timeout: u64,

    /// Requests-per-minute throttle. Free tier is ~15 RPM for flash-lite, so 12
    /// is safe; raise it (e.g. 300) once billing is enabled.
    #[arg(long, default_value_t = 12)]
    rpm: u64,

    /// Log file path (default: <output>.log). Info level, or per-call detail with --verbose.
    #[arg(long)]
    log: Option<PathBuf>,

    /// Verbose logging — record every Gemini API attempt and retry in the log.
    #[arg(long)]
    verbose: bool,

    /// Ignore the cache and re-extract every file.
    #[arg(long)]
    no_cache: bool,

    /// Process at most N files (handy for a quick trial).
    #[arg(long)]
    limit: Option<usize>,
}

#[tokio::main]
async fn main() -> Result<()> {
    let _ = dotenvy::dotenv();
    // Silence the noisy (caught) pdf-extract panics; keep all others.
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let msg = info.to_string();
        if msg.contains("unicode map") || msg.contains("pdf-extract") {
            return;
        }
        default_hook(info);
    }));
    let cli = Cli::parse();

    let mut settings = Settings {
        workers: cli.workers,
        max_concurrent: cli.max_concurrent,
        resume_timeout_secs: cli.timeout,
        requests_per_minute: cli.rpm,
        ..Settings::default()
    };
    if let Some(m) = &cli.model {
        settings.model = m.clone();
        settings.verifier_model = m.clone();
    }

    // File logging (default <output>.log). --verbose adds per-API-call detail.
    let log_path = cli.log.clone().unwrap_or_else(|| cli.output.with_extension("log"));
    let level = if cli.verbose { log::LevelFilter::Debug } else { log::LevelFilter::Info };
    // Keep the log focused on our own events (429s, retries, per-resume outcomes)
    // rather than noisy HTTP/TLS internals.
    let log_cfg = simplelog::ConfigBuilder::new()
        .add_filter_ignore_str("hyper")
        .add_filter_ignore_str("hyper_util")
        .add_filter_ignore_str("reqwest")
        .add_filter_ignore_str("rustls")
        .add_filter_ignore_str("native_tls")
        .add_filter_ignore_str("h2")
        .add_filter_ignore_str("mio")
        .add_filter_ignore_str("want")
        .add_filter_ignore_str("tokio_util")
        .build();
    if let Ok(file) = std::fs::File::create(&log_path) {
        let _ = simplelog::WriteLogger::init(level, log_cfg, file);
    }
    log::info!(
        "run start: folder={} model={} rpm={} workers={} timeout={}s",
        cli.folder.display(), settings.model, settings.requests_per_minute,
        settings.workers, settings.resume_timeout_secs
    );

    let key = GeminiClient::api_key_from_env()
        .context("set GEMINI_API_KEY in the environment or a .env file")?;
    let client = GeminiClient::new(
        key,
        settings.max_retries,
        settings.max_concurrent,
        settings.requests_per_minute,
    )?;

    // discover files
    let mut files = util::discover_resumes(&cli.folder);
    if let Some(n) = cli.limit {
        files.truncate(n);
    }
    if files.is_empty() {
        anyhow::bail!("no supported resumes found under {}", cli.folder.display());
    }
    println!("Found {} resume(s) under {}", files.len(), cli.folder.display());

    // store
    let store_path = cli.store.clone().unwrap_or_else(|| {
        cli.output
            .parent()
            .unwrap_or_else(|| std::path::Path::new("."))
            .join("resume_store.sqlite")
    });
    let store = Store::open(&store_path).context("opening store")?;

    // partition into cached vs to-process
    let mut cached: Vec<CandidateRecord> = Vec::new();
    let mut todo: Vec<(PathBuf, String)> = Vec::new();
    for path in &files {
        let hash = util::hash_file(path).with_context(|| format!("hashing {}", path.display()))?;
        if !cli.no_cache && store.contains_hash(&hash)? {
            if let Some(rec) = store.get(&hash)? {
                cached.push(rec);
                continue;
            }
        }
        todo.push((path.clone(), hash));
    }
    println!(
        "  {} cached, {} to extract via {}",
        cached.len(),
        todo.len(),
        settings.model
    );

    // process concurrently
    let pb = ProgressBar::new(todo.len() as u64);
    pb.set_style(
        ProgressStyle::with_template("  [{bar:30}] {pos}/{len} {msg}")
            .unwrap_or_else(|_| ProgressStyle::default_bar())
            .progress_chars("=>-"),
    );

    let workers = settings.workers.max(1);
    let mut fresh: Vec<CandidateRecord> = Vec::new();
    let mut failures: Vec<String> = Vec::new();
    let t0 = std::time::Instant::now();

    // Each resume runs in its own spawned task so ANY unforeseen panic is
    // isolated to that one file (surfaced as a failure) and can never abort the
    // batch. buffer_unordered keeps at most `workers` tasks live at a time.
    let mut buffered = stream::iter(todo.into_iter().map(|(path, hash)| {
        let client = client.clone();
        let settings = settings.clone();
        let timeout = std::time::Duration::from_secs(settings.resume_timeout_secs);
        async move {
            let name = path
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default();
            let joined = tokio::spawn(async move {
                // Hard wall-clock cap: no single resume can stall the batch.
                match tokio::time::timeout(
                    timeout,
                    pipeline::process_one(&client, &settings, &path, hash),
                )
                .await
                {
                    Ok(res) => res,
                    Err(_) => Err(anyhow::anyhow!(
                        "timed out after {}s (abandoned)",
                        timeout.as_secs()
                    )),
                }
            })
            .await;
            (name, joined)
        }
    }))
    .buffer_unordered(workers);

    while let Some((name, joined)) = buffered.next().await {
        pb.inc(1);
        match joined {
            Ok(Ok(rec)) => {
                log::info!(
                    "ok: {name} -> {} | {} | {:.1}y | {} [{}]",
                    rec.name, rec.primary_role.label(), rec.years_experience,
                    rec.experience_level.label(), rec.status.label()
                );
                pb.set_message(rec.name.clone());
                if let Err(e) = store.upsert(&rec) {
                    log::warn!("store upsert failed for {name}: {e:#}");
                    eprintln!("  warn: store upsert failed for {name}: {e:#}");
                }
                fresh.push(rec);
            }
            Ok(Err(e)) => {
                log::warn!("FAILED {name}: {e:#}");
                failures.push(format!("{name}: {e:#}"));
            }
            Err(join_err) => {
                log::warn!("PANIC {name}: {join_err}");
                failures.push(format!("{name}: internal error while parsing ({join_err})"));
            }
        }
    }
    pb.finish_and_clear();
    let elapsed = t0.elapsed();
    log::info!(
        "run done: {} ok, {} failed, {:.1}s",
        fresh.len(), failures.len(), elapsed.as_secs_f64()
    );

    // combine this run's records, dedupe by identity
    let cached_count = cached.len();
    let mut run: Vec<CandidateRecord> = cached;
    run.extend(fresh);
    let extracted_ok = run.len() - cached_count;
    let records = dedupe(run);

    // write outputs
    let csv = cli.output.with_extension("csv");
    let json = cli.output.with_extension("json");
    if let Err(e) = resume_parser::excel::write_all(&records, &cli.output, &csv, &json) {
        eprintln!("  warn: {e:#}");
    }

    // summary + KPIs
    print_summary(&records, &failures, &cli.output, &csv, &json, &store_path, &log_path);
    let stats = RunStats {
        total_files: files.len(),
        cached: cached_count,
        extracted_ok,
        failures: failures.len(),
        timeouts: failures.iter().filter(|f| f.contains("timed out")).count(),
        elapsed,
        calls: client.calls.load(Ordering::Relaxed),
        in_tokens: client.prompt_tokens.load(Ordering::Relaxed),
        out_tokens: client.output_tokens.load(Ordering::Relaxed),
    };
    let kpis = metrics::report(&stats, &records);
    let metrics_path = cli.output.with_extension("metrics.json");
    if let Ok(f) = std::fs::File::create(&metrics_path) {
        let _ = serde_json::to_writer_pretty(f, &kpis);
        println!("  Metrics: {}", metrics_path.display());
    }
    Ok(())
}

/// Keep one record per candidate identity, preferring human-verified then
/// higher-confidence entries.
fn dedupe(mut run: Vec<CandidateRecord>) -> Vec<CandidateRecord> {
    let mut by_id: BTreeMap<String, CandidateRecord> = BTreeMap::new();
    for rec in run.drain(..) {
        let id = Store::identity(&rec);
        match by_id.get(&id) {
            Some(existing) if !better(&rec, existing) => {}
            _ => {
                by_id.insert(id, rec);
            }
        }
    }
    let mut out: Vec<CandidateRecord> = by_id.into_values().collect();
    // review-needed rows first, then by name
    out.sort_by(|a, b| {
        let ra = matches!(a.status, RowStatus::NeedsReview);
        let rb = matches!(b.status, RowStatus::NeedsReview);
        rb.cmp(&ra).then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
    out
}

/// Whether `a` should replace `b` as the kept record for a shared identity.
/// Fully deterministic (independent of processing order) so de-dup is idempotent:
/// human-verified > higher confidence > more fields filled > smaller filename.
fn better(a: &CandidateRecord, b: &CandidateRecord) -> bool {
    if a.human_verified != b.human_verified {
        return a.human_verified;
    }
    let (ra, rb) = (a.overall_confidence.rank(), b.overall_confidence.rank());
    if ra != rb {
        return ra > rb;
    }
    let (fa, fb) = (filled(a), filled(b));
    if fa != fb {
        return fa > fb;
    }
    a.source_file < b.source_file
}

/// Count of populated key fields, for the de-dup tie-break.
fn filled(r: &CandidateRecord) -> usize {
    let mut n = 0;
    if !r.name.is_empty() {
        n += 1;
    }
    if !r.email.is_empty() {
        n += 1;
    }
    if !r.whatsapp.is_empty() {
        n += 1;
    }
    if r.current_company != "—" && !r.current_company.is_empty() {
        n += 1;
    }
    if !r.key_skills.is_empty() {
        n += 1;
    }
    n
}

#[allow(clippy::too_many_arguments)]
fn print_summary(
    records: &[CandidateRecord],
    failures: &[String],
    xlsx: &std::path::Path,
    csv: &std::path::Path,
    json: &std::path::Path,
    store: &std::path::Path,
    log: &std::path::Path,
) {
    let review = records
        .iter()
        .filter(|r| matches!(r.status, RowStatus::NeedsReview))
        .count();
    let verified = records.len() - review;

    println!("\n── Done ─────────────────────────────────────");
    println!("  Candidates: {}  ({} ✅ verified, {} ⚠️ need review)", records.len(), verified, review);
    if !failures.is_empty() {
        println!("  Failed to parse: {}", failures.len());
        for f in failures {
            println!("     - {f}");
        }
    }
    println!("  Excel : {}", xlsx.display());
    println!("  CSV   : {}", csv.display());
    println!("  JSON  : {}", json.display());
    println!("  Store : {}", store.display());
    println!("  Log   : {}", log.display());
    if !failures.is_empty() {
        println!("  ⓘ 429s / rate limits? see the Log above; on free tier add e.g. --rpm 8, or enable billing.");
    }
    if review > 0 {
        println!(
            "\n  {review} row(s) flagged ⚠️ — they are sorted to the top of the Excel; \
             hover the coloured cells for the evidence, confirm or fix, and they're done."
        );
    }
}
