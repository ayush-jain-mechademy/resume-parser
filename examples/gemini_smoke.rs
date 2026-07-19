//! Smoke test: one real structured Gemini call. Run with:
//!   cargo run --example gemini_smoke

use resume_parser::gemini::{GeminiClient, GenRequest};
use serde::Deserialize;
use serde_json::json;
use std::sync::atomic::Ordering;

#[derive(Debug, Deserialize)]
struct Mini {
    name: Option<String>,
    primary_role: Option<String>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let _ = dotenvy::dotenv();
    let key = GeminiClient::api_key_from_env()?;
    let client = GeminiClient::new(key, 4, 5, 60)?;

    let resume = std::fs::read_to_string("sample_resumes/bob_backend_employed.txt")?;
    let schema = json!({
        "type": "OBJECT",
        "properties": {
            "name": {"type": "STRING"},
            "primary_role": {"type": "STRING", "enum": ["Backend","Frontend","Full Stack","Other"]}
        },
        "required": ["name","primary_role"]
    });

    let out: Mini = client
        .generate_json(GenRequest {
            model: "gemini-3.1-flash-lite",
            system: Some("You extract fields from resumes. Return only the schema JSON."),
            prompt: format!("Resume:\n{resume}"),
            schema,
            pdf: None,
            temperature: 0.0,
        })
        .await?;

    println!("Parsed: {out:?}");
    println!(
        "tokens: prompt={} output={} calls={}",
        client.prompt_tokens.load(Ordering::Relaxed),
        client.output_tokens.load(Ordering::Relaxed),
        client.calls.load(Ordering::Relaxed),
    );
    Ok(())
}
