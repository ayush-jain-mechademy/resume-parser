//! Orchestration: for one resume, run the specialist agents concurrently, apply
//! 2-pass consensus on the error-prone ones, reconcile against the deterministic
//! layer (regex contact + temporal math + evidence checks), run the Verifier,
//! and gate every field's confidence into a trustworthy `CandidateRecord`.

use crate::config::{ExperienceLevel, Role, Settings};
use crate::schema::*;
use crate::{agents, deterministic as det};
use crate::gemini::GeminiClient;
use anyhow::Result;
use chrono::{Datelike, NaiveDate};
use serde_json::json;
use std::collections::BTreeMap;
use std::path::Path;

/// Column keys that drive overall confidence and the review gate.
const KEY_FIELDS: [&str; 9] = [
    "Name",
    "Email",
    "WhatsApp/Phone",
    "Currently Working At",
    "Currently Employed?",
    "Gap?",
    "Years of Experience",
    "Fresher/Experienced",
    "Primary Role",
];

fn today() -> NaiveDate {
    chrono::Local::now().date_naive()
}

fn ok_or_flag<T: Default>(r: Result<T>, label: &str, reasons: &mut Vec<String>) -> T {
    match r {
        Ok(v) => v,
        Err(e) => {
            reasons.push(format!("{label} agent failed: {e}"));
            T::default()
        }
    }
}

/// Process a single resume into a fully-reconciled record.
pub async fn process_one(
    client: &GeminiClient,
    settings: &Settings,
    path: &Path,
    file_hash: String,
) -> Result<CandidateRecord> {
    let source_file = path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();

    let ex = crate::ingest::extract(path, settings.min_text_chars)?;
    let text = ex.text.clone();
    let pdf: Option<&[u8]> = ex.pdf_bytes.as_deref();
    let model = settings.model.as_str();

    let mut reasons: Vec<String> = Vec::new();
    let today_str = today().format("%Y-%m-%d").to_string();

    // --- run specialists concurrently (2 passes for employment + role) -------
    let (contact_r, emp1_r, emp2_r, role1_r, role2_r, skills_r) = tokio::join!(
        agents::contact(client, model, &text, pdf),
        agents::employment(client, model, &text, pdf, 0.0, &today_str),
        agents::employment(client, model, &text, pdf, 0.6, &today_str),
        agents::role(client, model, &text, pdf, 0.0),
        agents::role(client, model, &text, pdf, 0.6),
        agents::skills(client, model, &text, pdf),
    );

    let contact = ok_or_flag(contact_r, "Contact", &mut reasons);
    let emp1 = ok_or_flag(emp1_r, "Employment", &mut reasons);
    let emp2 = emp2_r.unwrap_or_default();
    let role1 = ok_or_flag(role1_r, "Role", &mut reasons);
    let role2 = role2_r.unwrap_or_default();
    let skills = skills_r.unwrap_or_default();

    let mut field_conf: BTreeMap<String, Confidence> = BTreeMap::new();
    let mut evidence: BTreeMap<String, String> = BTreeMap::new();

    // --- contact reconciliation (regex is source of truth) -------------------
    let emails = det::find_emails(&text);
    let phones = det::find_phones(&text);

    let (email, email_conf) = reconcile_contact(&emails, contact.email.as_deref(), &text, true);
    let (whatsapp, phone_conf) = reconcile_contact(&phones, contact.phone.as_deref(), &text, false);
    field_conf.insert("Email".into(), email_conf);
    field_conf.insert("WhatsApp/Phone".into(), phone_conf);
    if email.is_empty() {
        reasons.push("no email found".into());
    }
    if whatsapp.is_empty() {
        reasons.push("no phone found".into());
    }

    // --- name (with filename fallback) ---------------------------------------
    let mut name = contact.name.clone().unwrap_or_default().trim().to_string();
    let name_ev = contact
        .name_evidence
        .clone()
        .or_else(|| contact.name.clone())
        .unwrap_or_default();
    let mut name_from_file = false;
    if name.is_empty() {
        let fallback = name_from_filename(&source_file);
        if !fallback.is_empty() {
            name = fallback;
            name_from_file = true;
        }
    }
    let name_conf = if name.is_empty() {
        reasons.push("no name found".into());
        Confidence::Low
    } else if name_from_file {
        reasons.push("name inferred from filename (not found in resume text)".into());
        Confidence::Low
    } else if text.trim().is_empty() {
        Confidence::Medium // vision-only, can't verify verbatim
    } else if det::evidence_supported(&name_ev, &text) || det::evidence_supported(&name, &text) {
        Confidence::High
    } else {
        reasons.push(format!("name \"{name}\" not found verbatim in resume"));
        Confidence::Medium
    };
    field_conf.insert("Name".into(), name_conf);
    if !name_ev.is_empty() {
        evidence.insert("Name".into(), name_ev);
    }

    // --- employment: normalize both passes, then MERGE ----------------------
    // Small models sometimes drop a job in one pass. Rather than trust a single
    // pass, we merge the two histories (recovering anything either pass missed)
    // and derive from the union. Disagreement between passes ⇒ flag for review.
    let today_date = today();
    let jobs1 = det::normalize_jobs(&emp1.jobs);
    let jobs2 = det::normalize_jobs(&emp2.jobs);
    let jobs_m = merge_jobs(&jobs1, &jobs2);
    let d1 = det::derive_employment(&jobs1, today_date, settings.gap_flag_months);
    let d2 = det::derive_employment(&jobs2, today_date, settings.gap_flag_months);
    let d = det::derive_employment(&jobs_m, today_date, settings.gap_flag_months);

    let both_nonempty = !emp1.jobs.is_empty() && !emp2.jobs.is_empty();
    let emp_agree = both_nonempty && employment_agrees(&d1, &d2);
    let emp_conf = if emp_agree && d.notes.is_empty() {
        Confidence::High
    } else {
        if !emp_agree {
            reasons.push(
                "employment history differed between model passes — used merged history".into(),
            );
        }
        for n in &d.notes {
            reasons.push(format!("date issue: {n}"));
        }
        Confidence::Medium
    };

    let current_company = d.current_company.clone().unwrap_or_default();
    let company_display = if current_company.is_empty() {
        "—".to_string()
    } else {
        current_company.clone()
    };
    // company evidence: the current job's evidence line, if present
    if let Some(cur) = jobs_m.iter().find(|j| j.is_current) {
        if !cur.evidence.is_empty() {
            evidence.insert("Currently Working At".into(), cur.evidence.clone());
        }
    }

    field_conf.insert("Currently Working At".into(), company_conf(&d, emp_conf));
    field_conf.insert("Currently Employed?".into(), emp_conf);
    field_conf.insert("Gap?".into(), emp_conf);
    field_conf.insert("Years of Experience".into(), emp_conf);
    field_conf.insert("Fresher/Experienced".into(), emp_conf);

    // --- role: consensus, with a deterministic keyword fallback --------------
    let primary_label = role1.primary_role.clone().unwrap_or_default();
    let secondary_role = role1
        .secondary_role
        .as_deref()
        .filter(|s| !s.eq_ignore_ascii_case("none") && !s.is_empty())
        .map(Role::from_label);
    let (primary_role, role_conf) = if primary_label.is_empty() {
        // Role agent gave nothing → infer from skills/keywords so a role is
        // still captured rather than left blank.
        let (kw_role, score) = det::classify_role_by_keywords(&text);
        if score > 0 {
            reasons.push("primary role inferred from skills (role agent returned none)".into());
            (kw_role, Confidence::Low)
        } else {
            reasons.push("could not determine primary role".into());
            (Role::Other, Confidence::Low)
        }
    } else {
        let pr = Role::from_label(&primary_label);
        let role2_primary = role2.primary_role.clone().unwrap_or_default();
        let roles_agree = primary_label.eq_ignore_ascii_case(&role2_primary);
        let model_role_conf = role1
            .confidence
            .as_deref()
            .map(Confidence::from_str_loose)
            .unwrap_or(Confidence::Medium);
        let conf = if roles_agree && model_role_conf == Confidence::High {
            Confidence::High
        } else if roles_agree {
            Confidence::Medium
        } else {
            reasons.push(format!(
                "role varied between passes ({primary_label} vs {role2_primary})"
            ));
            Confidence::Low
        };
        (pr, conf)
    };
    field_conf.insert("Primary Role".into(), role_conf);
    if let Some(ev) = &role1.evidence {
        if !ev.is_empty() {
            evidence.insert("Primary Role".into(), ev.clone());
        }
    }

    // --- assemble record -----------------------------------------------------
    let mut rec = CandidateRecord {
        source_file,
        file_hash,
        name,
        whatsapp,
        email,
        current_company: company_display,
        currently_employed: d.currently_employed,
        has_gap: d.has_gap,
        gap_duration: d.gap_duration.clone(),
        gap_months: d.gap_months,
        years_experience: d.years_experience,
        experience_level: d.experience_level,
        primary_role,
        secondary_role,
        key_skills: skills.skills,
        status: RowStatus::NeedsReview,
        overall_confidence: Confidence::Low,
        review_reasons: Vec::new(),
        field_confidence: field_conf,
        evidence,
        jobs: jobs_m,
        human_verified: false,
        used_vision: ex.needs_vision,
    };

    // --- cross-field sanity --------------------------------------------------
    sanity_checks(&rec, &mut reasons);

    // --- Verifier agent ------------------------------------------------------
    // Present absent values as JSON null so the verifier doesn't flag our "—"
    // placeholders as "not in the resume".
    let nz = |s: &str| -> serde_json::Value {
        if s.is_empty() || s == "—" { serde_json::Value::Null } else { json!(s) }
    };
    let summary = json!({
        "name": nz(&rec.name),
        "email": nz(&rec.email),
        "whatsapp": nz(&rec.whatsapp),
        "current_company": if rec.currently_employed { nz(&rec.current_company) } else { serde_json::Value::Null },
        "currently_employed": rec.currently_employed,
        "has_gap": rec.has_gap,
        "gap_duration": if rec.has_gap { json!(rec.gap_duration) } else { serde_json::Value::Null },
        "years_experience": rec.years_experience,
        "experience_level": rec.experience_level.label(),
        "primary_role": rec.primary_role.label(),
    });
    if let Ok(v) =
        agents::verifier(client, &settings.verifier_model, &text, pdf, &summary, &today_str).await
    {
        apply_verifier(&v, &mut rec.field_confidence, &mut reasons);
    }

    // ensure every key field has a confidence entry
    for k in KEY_FIELDS {
        rec.field_confidence.entry(k.to_string()).or_insert(Confidence::Medium);
    }

    reasons.sort();
    reasons.dedup();
    rec.review_reasons = reasons;
    rec.finalize_status();
    Ok(rec)
}

/// Pick the best contact value: prefer a regex hit (guaranteed in-source);
/// fall back to the agent's value only if it, too, appears in the source.
fn reconcile_contact(
    regex_hits: &[String],
    agent_val: Option<&str>,
    source: &str,
    is_email: bool,
) -> (String, Confidence) {
    if let Some(first) = regex_hits.first() {
        return (first.clone(), Confidence::High);
    }
    if let Some(a) = agent_val {
        let a = a.trim();
        if !a.is_empty() {
            let normalized = if is_email {
                Some(a.to_string())
            } else {
                det::normalize_phone(a)
            };
            if let Some(val) = normalized {
                let supported = if is_email {
                    det::contains_email(source, &val)
                } else {
                    det::contains_phone(source, &val)
                };
                // In source (or source is empty => vision-only, unverifiable).
                let conf = if supported {
                    Confidence::Medium
                } else if source.trim().is_empty() {
                    Confidence::Medium
                } else {
                    Confidence::Low
                };
                return (val, conf);
            }
        }
    }
    (String::new(), Confidence::Low)
}

/// Union two job histories, deduping by company + start-year (or matching
/// title). When the same role appears in both, prefer the reading that is
/// ongoing / has a start date, so we never lose "currently employed".
fn merge_jobs(a: &[Job], b: &[Job]) -> Vec<Job> {
    let mut out: Vec<Job> = a.to_vec();
    for jb in b {
        if let Some(pos) = out.iter().position(|ja| same_job(ja, jb)) {
            if (jb.is_current && !out[pos].is_current)
                || (out[pos].start.is_none() && jb.start.is_some())
            {
                out[pos] = jb.clone();
            }
        } else {
            out.push(jb.clone());
        }
    }
    out
}

fn same_job(a: &Job, b: &Job) -> bool {
    let ca = norm_ident(&a.company);
    let cb = norm_ident(&b.company);
    if ca.is_empty() || ca != cb {
        return false;
    }
    let start_same = match (a.start, b.start) {
        (Some(x), Some(y)) => x.year() == y.year(),
        (None, None) => true,
        _ => false,
    };
    start_same || norm_ident(&a.title) == norm_ident(&b.title)
}

fn norm_ident(s: &str) -> String {
    s.to_ascii_lowercase().chars().filter(|c| c.is_alphanumeric()).collect()
}

/// Best-effort candidate name from a resume filename, e.g.
/// "Rajat_Newatia_Resume-v2.pdf" → "Rajat Newatia". Drops resume-ish junk words
/// and anything containing digits; keeps the first three name-like tokens.
fn name_from_filename(file: &str) -> String {
    const JUNK: [&str; 14] = [
        "resume", "cv", "final", "updated", "new", "copy", "doc", "docx", "pdf",
        "mechademy", "sde", "profile", "curriculum", "vitae",
    ];
    let stem = file.rsplit_once('.').map(|(s, _)| s).unwrap_or(file);
    stem.split(|c: char| c == '_' || c == '-' || c == ' ' || c == '.')
        .filter(|w| !w.is_empty())
        .filter(|w| !w.chars().any(|c| c.is_ascii_digit()))
        .filter(|w| !JUNK.contains(&w.to_ascii_lowercase().as_str()))
        .take(3)
        .map(|w| {
            let mut chars = w.chars();
            match chars.next() {
                Some(f) => f.to_uppercase().collect::<String>() + &chars.as_str().to_lowercase(),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
        .trim()
        .to_string()
}

fn employment_agrees(a: &det::Derived, b: &det::Derived) -> bool {
    a.currently_employed == b.currently_employed
        && a.has_gap == b.has_gap
        && (a.years_experience - b.years_experience).abs() <= 1.0
        && norm_company(&a.current_company) == norm_company(&b.current_company)
}

fn norm_company(c: &Option<String>) -> String {
    c.as_deref()
        .unwrap_or("")
        .to_ascii_lowercase()
        .chars()
        .filter(|ch| ch.is_alphanumeric())
        .collect()
}

fn company_conf(d: &det::Derived, base: Confidence) -> Confidence {
    if d.currently_employed && d.current_company.as_deref().unwrap_or("").is_empty() {
        Confidence::Low
    } else {
        base
    }
}

fn sanity_checks(rec: &CandidateRecord, reasons: &mut Vec<String>) {
    if rec.experience_level == ExperienceLevel::Fresher && rec.years_experience > 1.5 {
        reasons.push(format!(
            "marked Fresher but {} yrs experience computed",
            rec.years_experience
        ));
    }
    if rec.currently_employed && !rec.jobs.iter().any(|j| j.is_current) {
        reasons.push("marked employed but no ongoing role".into());
    }
    if rec.has_gap && rec.currently_employed {
        reasons.push("has-gap and currently-employed conflict".into());
    }
    // A very long current gap is either genuine (worth noting) or a sign the
    // model missed recent roles — either way a human should glance at it.
    if rec.has_gap && rec.gap_months > 36 {
        reasons.push(format!(
            "unusually long current gap ({}) — verify recent roles weren't missed",
            rec.gap_duration
        ));
    }
    if rec.primary_role == Role::Other {
        reasons.push("primary role classified as Other".into());
    }
}

fn apply_verifier(
    v: &VerifierExtraction,
    field_conf: &mut BTreeMap<String, Confidence>,
    reasons: &mut Vec<String>,
) {
    for issue in &v.issues {
        let problem = issue.problem.clone().unwrap_or_default();
        if problem.trim().is_empty() {
            continue;
        }
        let field = issue.field.clone().unwrap_or_default();
        let sev = issue
            .severity
            .as_deref()
            .map(Confidence::from_str_loose)
            .unwrap_or(Confidence::Medium);
        reasons.push(format!("verifier: {field}: {problem}"));
        if let Some(col) = map_verifier_field(&field) {
            let downgrade = if sev == Confidence::High {
                Confidence::Low
            } else {
                Confidence::Medium
            };
            let cur = field_conf.get(col).copied().unwrap_or(Confidence::Medium);
            if downgrade.rank() < cur.rank() {
                field_conf.insert(col.to_string(), downgrade);
            }
        }
    }
}

fn map_verifier_field(field: &str) -> Option<&'static str> {
    let f = field.to_ascii_lowercase();
    if f.contains("email") {
        Some("Email")
    } else if f.contains("phone") || f.contains("whatsapp") || f.contains("mobile") {
        Some("WhatsApp/Phone")
    } else if f.contains("name") {
        Some("Name")
    } else if f.contains("gap") {
        Some("Gap?")
    } else if f.contains("year") || f.contains("experience") {
        Some("Years of Experience")
    } else if f.contains("role") {
        Some("Primary Role")
    } else if f.contains("compan") || f.contains("employ") {
        Some("Currently Working At")
    } else {
        None
    }
}
