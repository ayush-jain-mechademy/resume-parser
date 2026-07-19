//! The specialist extraction agents. Each is a focused Gemini call with its own
//! response schema and a shared "extract only what's present, always quote your
//! evidence, never invent" system instruction. Splitting by field-group keeps
//! each call focused and robust to the many ways resumes phrase things.

use crate::config::Role;
use crate::gemini::{GeminiClient, GenRequest};
use crate::schema::*;
use anyhow::Result;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

const SYSTEM: &str = "You are an expert resume-parsing agent for IT recruiting. \
Extract ONLY information explicitly present in the resume — never guess, infer a \
value that isn't stated, or fabricate contact details. For every extracted value, \
copy the exact supporting text from the resume into the corresponding `evidence` \
field. If something is genuinely absent, return null or an empty list. Respond \
with strictly and only the requested JSON.";

/// Cap prompt size; resumes are small but guard against pathological inputs.
const MAX_TEXT: usize = 24_000;

fn resume_block(text: &str, has_pdf: bool) -> String {
    if has_pdf && text.trim().is_empty() {
        "The resume is attached as a PDF document. Read it directly.".to_string()
    } else {
        let t: String = text.chars().take(MAX_TEXT).collect();
        format!("Resume text:\n\"\"\"\n{t}\n\"\"\"")
    }
}

async fn run<T: DeserializeOwned>(
    client: &GeminiClient,
    model: &str,
    prompt: String,
    schema: Value,
    pdf: Option<&[u8]>,
    temperature: f32,
) -> Result<T> {
    client
        .generate_json(GenRequest {
            model,
            system: Some(SYSTEM),
            prompt,
            schema,
            pdf,
            temperature,
        })
        .await
}

fn nullable_str() -> Value {
    json!({"type": "STRING", "nullable": true})
}

// ---------------------------------------------------------------------------
// Contact
// ---------------------------------------------------------------------------

pub async fn contact(
    client: &GeminiClient,
    model: &str,
    text: &str,
    pdf: Option<&[u8]>,
) -> Result<ContactExtraction> {
    let schema = json!({
        "type": "OBJECT",
        "properties": {
            "name": nullable_str(),
            "name_evidence": nullable_str(),
            "email": nullable_str(),
            "phone": nullable_str(),
            "location": nullable_str(),
            "linkedin": nullable_str(),
            "github": nullable_str()
        }
    });
    let prompt = format!(
        "Extract the candidate's contact details.\n\
         - `name`: the PERSON's full name (usually at the very top). Never use a \
         company, product, or section-heading as the name. Put the exact source \
         line in `name_evidence`.\n\
         - `email`, `phone`: exactly as written. `phone` = the primary mobile/\
         WhatsApp number if several are present.\n\
         - `location`, `linkedin`, `github`: if present.\n\n{}",
        resume_block(text, pdf.is_some())
    );
    run(client, model, prompt, schema, pdf, 0.0).await
}

// ---------------------------------------------------------------------------
// Employment history (the backbone)
// ---------------------------------------------------------------------------

pub async fn employment(
    client: &GeminiClient,
    model: &str,
    text: &str,
    pdf: Option<&[u8]>,
    temperature: f32,
    today: &str,
) -> Result<EmploymentExtraction> {
    let job = json!({
        "type": "OBJECT",
        "properties": {
            "company": nullable_str(),
            "title": nullable_str(),
            "start": nullable_str(),
            "end": nullable_str(),
            "is_current": {"type": "BOOLEAN", "nullable": true},
            "employment_type": {
                "type": "STRING",
                "enum": ["full-time", "internship", "freelance", "contract", "unknown"],
                "nullable": true
            },
            "evidence": nullable_str()
        }
    });
    let schema = json!({
        "type": "OBJECT",
        "properties": { "jobs": {"type": "ARRAY", "items": job} }
    });
    let prompt = format!(
        "Today's date is {today}. Use it when judging whether a role is ongoing.\n\
         Extract EVERY work-experience entry (paid jobs, internships, freelance, \
         contract). EXCLUDE education, academic projects, certifications, awards, \
         and volunteering.\n\
         For each entry:\n\
         - `company`, `title`.\n\
         - `start`, `end`: dates EXACTLY as written in the resume (e.g. \"Jan 2020\", \
         \"03/2021\", \"2019\"). Do not reformat.\n\
         - If the role is ongoing (says Present/Current/Till Date, or has NO end \
         date and is the latest role), set `end` to \"Present\" and `is_current` true.\n\
         - CRITICAL: if an explicit end date IS given — even a recent one like \
         2025 — the role is NOT ongoing: set `is_current` false and keep that end \
         date. Never mark a role current just because it is the most recent one.\n\
         - `employment_type`: one of full-time/internship/freelance/contract/unknown. \
         Titles containing \"Intern\" are internships.\n\
         - `evidence`: the exact line(s) from the resume for this entry.\n\
         Order most-recent first.\n\n{}",
        resume_block(text, pdf.is_some())
    );
    run(client, model, prompt, schema, pdf, temperature).await
}

// ---------------------------------------------------------------------------
// Role intent
// ---------------------------------------------------------------------------

pub async fn role(
    client: &GeminiClient,
    model: &str,
    text: &str,
    pdf: Option<&[u8]>,
    temperature: f32,
) -> Result<RoleExtraction> {
    let labels = Role::all_labels();
    let mut secondary: Vec<Value> = labels.iter().map(|l| json!(l)).collect();
    secondary.push(json!("None"));
    let schema = json!({
        "type": "OBJECT",
        "properties": {
            "primary_role": {"type": "STRING", "enum": labels},
            "secondary_role": {"type": "STRING", "enum": secondary, "nullable": true},
            "evidence": nullable_str(),
            "confidence": {"type": "STRING", "enum": ["high", "medium", "low"], "nullable": true}
        },
        "required": ["primary_role"]
    });
    let prompt = format!(
        "Determine the single IT role this candidate is PRIMARILY targeting / best \
         fits. Base it on their objective/headline, most recent job titles, and \
         skills — weight the most recent role and any stated objective most.\n\
         Choose exactly ONE `primary_role` from the allowed list, and optionally a \
         `secondary_role` (or \"None\"). Give supporting `evidence` and your \
         `confidence`.\n\
         Allowed roles: {}.\n\n{}",
        labels.join(", "),
        resume_block(text, pdf.is_some())
    );
    run(client, model, prompt, schema, pdf, temperature).await
}

// ---------------------------------------------------------------------------
// Skills
// ---------------------------------------------------------------------------

pub async fn skills(
    client: &GeminiClient,
    model: &str,
    text: &str,
    pdf: Option<&[u8]>,
) -> Result<SkillsExtraction> {
    let schema = json!({
        "type": "OBJECT",
        "properties": { "skills": {"type": "ARRAY", "items": {"type": "STRING"}} }
    });
    let prompt = format!(
        "List the candidate's technical skills / tools / technologies as a \
         deduplicated array of short tokens (e.g. \"React\", \"AWS\", \"PostgreSQL\"). \
         Only include skills actually present in the resume. Cap at ~25 most relevant.\n\n{}",
        resume_block(text, pdf.is_some())
    );
    run(client, model, prompt, schema, pdf, 0.0).await
}

// ---------------------------------------------------------------------------
// Education
// ---------------------------------------------------------------------------

pub async fn education(
    client: &GeminiClient,
    model: &str,
    text: &str,
    pdf: Option<&[u8]>,
) -> Result<EducationExtraction> {
    let item = json!({
        "type": "OBJECT",
        "properties": {
            "degree": nullable_str(),
            "institution": nullable_str(),
            "start": nullable_str(),
            "end": nullable_str(),
            "evidence": nullable_str()
        }
    });
    let schema = json!({
        "type": "OBJECT",
        "properties": { "education": {"type": "ARRAY", "items": item} }
    });
    let prompt = format!(
        "Extract education entries (degrees/diplomas only). For each: `degree`, \
         `institution`, `start` year and `end`/graduation year exactly as written, \
         and `evidence` (the exact line).\n\n{}",
        resume_block(text, pdf.is_some())
    );
    run(client, model, prompt, schema, pdf, 0.0).await
}

// ---------------------------------------------------------------------------
// Verifier / adjudicator
// ---------------------------------------------------------------------------

/// Audit the reconciled fields against the resume. `summary` is a compact JSON
/// of what the pipeline concluded.
pub async fn verifier(
    client: &GeminiClient,
    model: &str,
    text: &str,
    pdf: Option<&[u8]>,
    summary: &Value,
    today: &str,
) -> Result<VerifierExtraction> {
    let issue = json!({
        "type": "OBJECT",
        "properties": {
            "field": nullable_str(),
            "problem": nullable_str(),
            "severity": {"type": "STRING", "enum": ["high", "medium", "low"], "nullable": true}
        }
    });
    let schema = json!({
        "type": "OBJECT",
        "properties": { "issues": {"type": "ARRAY", "items": issue} }
    });
    let prompt = format!(
        "Today's date is {today}. You are auditing extracted resume data for \
         correctness. Below are the fields the system concluded, followed by the \
         resume.\n\
         IMPORTANT — division of labour:\n\
         - Do NOT recompute years-of-experience or gap durations yourself. Those are \
         computed deterministically by the system from the dates and are correct if \
         the dates are right. Never flag `years_experience` or `gap_duration` numeric \
         values.\n\
         - A role whose end date is BEFORE today's date ({today}) is finished, not \
         current — judge current-employment and gaps using today's date.\n\
         - The primary mobile number is intentionally the WhatsApp number; null/empty \
         means 'not present' — do not flag either. Phone numbers are normalized to \
         E.164, so an added country code like +91 is correct — never flag it.\n\
         - Only mid-career/current gaps matter; ignore gaps between education and the \
         first job. A Fresher (no full-time role yet) has NO employment gap by \
         definition — never flag has_gap/gap_duration for a fresher, and never treat \
         time since an internship or graduation as a gap. CRITICAL: `has_gap` reflects \
         ONLY a CURRENT gap (unemployed as of today). If currently_employed is true, \
         has_gap MUST be false — never flag a historical gap between two past jobs for \
         someone who is currently employed.\n\
         Report ONLY: a name/email/phone/company/title/date that is wrong or \
         unsupported by the resume, a current-vs-ended status that contradicts the \
         dates given today, or a clearly wrong primary role. Return an empty list if \
         the extracted facts match the resume. For each issue give `field`, `problem`, \
         `severity`.\n\n\
         Extracted fields (JSON):\n{}\n\n{}",
        serde_json::to_string_pretty(summary).unwrap_or_default(),
        resume_block(text, pdf.is_some())
    );
    run(client, model, prompt, schema, pdf, 0.0).await
}
