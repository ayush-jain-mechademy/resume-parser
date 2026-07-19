//! Minimal async Gemini REST client with structured-JSON output, native-PDF
//! (vision) support, retry/backoff, and token accounting for cost reporting.

use anyhow::{Context, Result, anyhow, bail};
use base64::Engine;
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

const API_BASE: &str = "https://generativelanguage.googleapis.com/v1beta";

#[derive(Clone)]
pub struct GeminiClient {
    http: reqwest::Client,
    api_key: String,
    max_retries: u32,
    /// Global cap on in-flight requests, independent of resume-level concurrency.
    /// Keeps free-tier rate limits from being blown by a burst of parallel calls.
    sema: Arc<tokio::sync::Semaphore>,
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
    pub fn new(api_key: String, max_retries: u32, max_concurrent: usize) -> Result<Self> {
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

        // Hold a global permit for the whole call (incl. backoff) so the fleet of
        // per-resume requests can't exceed the configured concurrency. If the
        // semaphore is ever closed we proceed unthrottled rather than panic.
        let _permit = self.sema.acquire().await.ok();

        let mut attempt = 0u32;
        loop {
            attempt += 1;
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
                    let text = r.text().await.unwrap_or_default();
                    if status.is_success() {
                        self.calls.fetch_add(1, Ordering::Relaxed);
                        return self.extract_text(&text);
                    }
                    // 429 / 5xx are transient; back off and retry.
                    let transient = status.as_u16() == 429 || status.is_server_error();
                    if transient && attempt <= self.max_retries {
                        backoff(attempt).await;
                        continue;
                    }
                    bail!("Gemini HTTP {}: {}", status, truncate(&text, 500));
                }
                Err(e) => {
                    if attempt <= self.max_retries {
                        backoff(attempt).await;
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

async fn backoff(attempt: u32) {
    // Exponential: 0.5s, 1s, 2s, 4s, ... capped at 16s.
    let secs = (0.5 * 2f64.powi(attempt as i32 - 1)).min(16.0);
    tokio::time::sleep(Duration::from_secs_f64(secs)).await;
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
}

#[derive(Deserialize)]
struct UsageMetadata {
    #[serde(rename = "promptTokenCount")]
    prompt_token_count: Option<u64>,
    #[serde(rename = "candidatesTokenCount")]
    candidates_token_count: Option<u64>,
}
