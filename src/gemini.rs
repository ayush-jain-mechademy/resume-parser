//! Minimal async Gemini REST client with structured-JSON output, native-PDF
//! (vision) support, retry/backoff, and token accounting for cost reporting.

use anyhow::{Context, Result, anyhow, bail};
use base64::Engine;
use log::{debug, warn};
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;
use tokio::sync::Mutex as AsyncMutex;
use tokio::time::Instant as TokioInstant;

const API_BASE: &str = "https://generativelanguage.googleapis.com/v1beta";

/// Minimum-interval rate limiter: enforces at most `rpm` requests per minute
/// across the whole process by handing each request a time slot spaced by
/// `60/rpm` seconds. This is what actually keeps us under free-tier RPM limits
/// (a concurrency cap alone does not — fast requests still exceed the rate).
struct RateLimiter {
    interval: Duration,
    next: AsyncMutex<TokioInstant>,
}

impl RateLimiter {
    fn new(rpm: u64) -> Self {
        let rpm = rpm.max(1);
        RateLimiter {
            interval: Duration::from_secs_f64(60.0 / rpm as f64),
            next: AsyncMutex::new(TokioInstant::now()),
        }
    }

    /// Wait until this caller's slot; returns immediately if we're under rate.
    async fn acquire(&self) {
        let scheduled = {
            let mut next = self.next.lock().await;
            let now = TokioInstant::now();
            let sched = if *next > now { *next } else { now };
            *next = sched + self.interval;
            sched
        };
        tokio::time::sleep_until(scheduled).await;
    }
}

#[derive(Clone)]
pub struct GeminiClient {
    http: reqwest::Client,
    api_key: String,
    max_retries: u32,
    /// Global cap on in-flight requests (memory/connection bound).
    sema: Arc<tokio::sync::Semaphore>,
    /// Requests-per-minute throttle (the real free-tier guard).
    rate: Arc<RateLimiter>,
    pub prompt_tokens: Arc<AtomicU64>,
    pub output_tokens: Arc<AtomicU64>,
    pub calls: Arc<AtomicU64>,
}

/// One structured-generation request.
pub struct GenRequest<'a> {
    pub model: &'a str,
    pub system: Option<&'a str>,
    pub prompt: String,
    /// Gemini response schema (OpenAPI subset) as JSON.
    pub schema: Value,
    /// When set, the PDF is attached so Gemini reads it natively (scanned docs).
    pub pdf: Option<&'a [u8]>,
    pub temperature: f32,
}

impl GeminiClient {
    pub fn new(
        api_key: String,
        max_retries: u32,
        max_concurrent: usize,
        rpm: u64,
    ) -> Result<Self> {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(60))
            .connect_timeout(Duration::from_secs(15))
            .build()
            .context("building http client")?;
        Ok(GeminiClient {
            http,
            api_key,
            max_retries,
            sema: Arc::new(tokio::sync::Semaphore::new(max_concurrent.max(1))),
            rate: Arc::new(RateLimiter::new(rpm)),
            prompt_tokens: Arc::new(AtomicU64::new(0)),
            output_tokens: Arc::new(AtomicU64::new(0)),
            calls: Arc::new(AtomicU64::new(0)),
        })
    }

    /// Load the API key from the environment (GEMINI_API_KEY / GOOGLE_API_KEY).
    pub fn api_key_from_env() -> Result<String> {
        std::env::var("GEMINI_API_KEY")
            .or_else(|_| std::env::var("GOOGLE_API_KEY"))
            .map_err(|_| anyhow!("GEMINI_API_KEY (or GOOGLE_API_KEY) is not set"))
    }

    /// Generate and deserialize into `T` (which must match the request schema).
    pub async fn generate_json<T: DeserializeOwned>(&self, req: GenRequest<'_>) -> Result<T> {
        let text = self.generate_raw(&req).await?;
        serde_json::from_str::<T>(&text)
            .with_context(|| format!("parsing Gemini JSON: {}", truncate(&text, 400)))
    }

    /// Generate raw text (the model's JSON string), with retry on transient errors.
    pub async fn generate_raw(&self, req: &GenRequest<'_>) -> Result<String> {
        let url = format!("{API_BASE}/models/{}:generateContent", req.model);
        let body = self.build_body(req);

        // In-flight cap (memory/connections). The RPM throttle below is the real
        // free-tier guard.
        let _permit = self.sema.acquire().await.ok();

        let mut attempt = 0u32;
        loop {
            attempt += 1;
            // Rate-limit EVERY attempt — retries also count against the quota.
            self.rate.acquire().await;
            debug!("→ {} attempt {}", req.model, attempt);

            let resp = self
                .http
                .post(&url)
                .header("x-goog-api-key", &self.api_key)
                .json(&body)
                .send()
                .await;

            match resp {
                Ok(r) => {
                    let status = r.status();
                    let retry_after = r
                        .headers()
                        .get("retry-after")
                        .and_then(|v| v.to_str().ok())
                        .and_then(|s| s.trim().parse::<f64>().ok());
                    let text = r.text().await.unwrap_or_default();
                    if status.is_success() {
                        self.calls.fetch_add(1, Ordering::Relaxed);
                        debug!("← {} 200 OK", req.model);
                        return self.extract_text(&text);
                    }
                    let code = status.as_u16();
                    let transient = code == 429 || status.is_server_error();
                    if transient && attempt <= self.max_retries {
                        let delay = retry_after
                            .or_else(|| retry_delay_secs(&text))
                            .unwrap_or_else(|| backoff_secs(attempt))
                            + jitter();
                        warn!(
                            "{} HTTP {} (attempt {}/{}) — retrying in {:.1}s. {}",
                            req.model,
                            code,
                            attempt,
                            self.max_retries + 1,
                            delay,
                            quota_hint(code, &text)
                        );
                        tokio::time::sleep(Duration::from_secs_f64(delay)).await;
                        continue;
                    }
                    warn!(
                        "{} HTTP {} — giving up after {} attempts. {}",
                        req.model,
                        code,
                        attempt,
                        truncate(&text, 300)
                    );
                    bail!("Gemini HTTP {}: {}", status, truncate(&text, 500));
                }
                Err(e) => {
                    if attempt <= self.max_retries {
                        let delay = backoff_secs(attempt) + jitter();
                        warn!(
                            "{} request error (attempt {}): {e} — retrying in {:.1}s",
                            req.model, attempt, delay
                        );
                        tokio::time::sleep(Duration::from_secs_f64(delay)).await;
                        continue;
                    }
                    return Err(e).context("Gemini request failed");
                }
            }
        }
    }

    fn build_body(&self, req: &GenRequest<'_>) -> Value {
        let mut parts: Vec<Value> = Vec::new();
        if let Some(bytes) = req.pdf {
            let b64 = base64::engine::general_purpose::STANDARD.encode(bytes);
            parts.push(json!({"inlineData": {"mimeType": "application/pdf", "data": b64}}));
        }
        parts.push(json!({ "text": req.prompt }));

        let mut body = json!({
            "contents": [{ "role": "user", "parts": parts }],
            "generationConfig": {
                "temperature": req.temperature,
                "responseMimeType": "application/json",
                "responseSchema": req.schema,
            }
        });
        if let Some(sys) = req.system {
            body["systemInstruction"] = json!({ "parts": [{ "text": sys }] });
        }
        body
    }

    fn extract_text(&self, raw: &str) -> Result<String> {
        let parsed: GenResponse =
            serde_json::from_str(raw).context("parsing Gemini envelope")?;

        if let Some(u) = &parsed.usage_metadata {
            self.prompt_tokens
                .fetch_add(u.prompt_token_count.unwrap_or(0), Ordering::Relaxed);
            self.output_tokens
                .fetch_add(u.candidates_token_count.unwrap_or(0), Ordering::Relaxed);
        }

        let cand = parsed
            .candidates
            .as_ref()
            .and_then(|c| c.first())
            .ok_or_else(|| {
                anyhow!(
                    "Gemini returned no candidates (blocked?): {}",
                    truncate(raw, 300)
                )
            })?;

        if let Some(reason) = &cand.finish_reason {
            if reason != "STOP" && reason != "MAX_TOKENS" {
                bail!("Gemini finishReason={reason}");
            }
        }

        let text = cand
            .content
            .as_ref()
            .and_then(|c| c.parts.as_ref())
            .map(|parts| {
                parts
                    .iter()
                    .filter(|p| p.thought != Some(true)) // skip 3.x "thought" parts
                    .filter_map(|p| p.text.as_deref())
                    .collect::<String>()
            })
            .unwrap_or_default();

        if text.trim().is_empty() {
            bail!("Gemini returned empty content");
        }
        Ok(text)
    }
}

/// Exponential backoff seconds, capped at 30s.
fn backoff_secs(attempt: u32) -> f64 {
    2f64.powi(attempt as i32).min(30.0)
}

/// Small random jitter (0–0.5s) to avoid synchronized retries.
fn jitter() -> f64 {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    (nanos % 500) as f64 / 1000.0
}

/// Parse Gemini's suggested `retryDelay` (e.g. "17s") from a 429 body.
fn retry_delay_secs(body: &str) -> Option<f64> {
    let v: Value = serde_json::from_str(body).ok()?;
    let details = v.get("error")?.get("details")?.as_array()?;
    for d in details {
        if let Some(rd) = d.get("retryDelay").and_then(|x| x.as_str()) {
            if let Ok(f) = rd.trim_end_matches('s').parse::<f64>() {
                return Some(f);
            }
        }
    }
    None
}

/// A short human hint about which quota was hit, for the log.
fn quota_hint(code: u16, body: &str) -> String {
    if code != 429 {
        return String::new();
    }
    let b = body.to_ascii_lowercase();
    if b.contains("per day") || b.contains("perday") || b.contains("requests per day") {
        "→ DAILY free-tier quota (RPD) exhausted — resets in ~24h, or enable billing.".into()
    } else if b.contains("per minute") || b.contains("perminute") || b.contains("per-minute") {
        "→ per-minute quota (RPM) — lower --rpm.".into()
    } else {
        "→ rate limited — lower --rpm, or enable billing for higher limits.".into()
    }
}

fn truncate(s: &str, n: usize) -> String {
    if s.len() <= n {
        return s.to_string();
    }
    // Back off to the nearest char boundary so we never slice mid-UTF-8
    // (resumes routinely contain accented names, ₹, em-dashes, non-Latin text).
    let mut end = n;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &s[..end])
}

// --- response envelope -----------------------------------------------------

#[derive(Deserialize)]
struct GenResponse {
    candidates: Option<Vec<Candidate>>,
    #[serde(rename = "usageMetadata")]
    usage_metadata: Option<UsageMetadata>,
}

#[derive(Deserialize)]
struct Candidate {
    content: Option<ContentBlock>,
    #[serde(rename = "finishReason")]
    finish_reason: Option<String>,
}

#[derive(Deserialize)]
struct ContentBlock {
    parts: Option<Vec<PartBlock>>,
}

#[derive(Deserialize)]
struct PartBlock {
    text: Option<String>,
    /// Gemini 3.x thinking models may return separate "thought" parts; these
    /// must be excluded so only the answer JSON is parsed.
    #[serde(default)]
    thought: Option<bool>,
}

#[derive(Deserialize)]
struct UsageMetadata {
    #[serde(rename = "promptTokenCount")]
    prompt_token_count: Option<u64>,
    #[serde(rename = "candidatesTokenCount")]
    candidates_token_count: Option<u64>,
}
