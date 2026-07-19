//! Deterministic reliability layer — no LLM, no cost. Regex/library contact
//! extraction, verbatim source verification, the temporal engine that computes
//! years-of-experience / gap / fresher from parsed dates, and evidence checks.
//! This is what turns "the model said so" into "the document proves it".

use crate::config::ExperienceLevel;
use crate::schema::{EmploymentType, Job, JobRaw};
use crate::util::humanize_months;
use chrono::{Datelike, NaiveDate};
use phonenumber::Mode;
use regex::Regex;
use std::sync::OnceLock;

// ---------------------------------------------------------------------------
// Contact extraction + verbatim verification
// ---------------------------------------------------------------------------

/// All e-mail addresses appearing in the text, in order, de-duplicated.
pub fn find_emails(text: &str) -> Vec<String> {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| {
        Regex::new(r"(?i)[a-z0-9._%+\-]+@[a-z0-9.\-]+\.[a-z]{2,}").unwrap()
    });
    let mut out = Vec::new();
    for m in re.find_iter(text) {
        let e = m.as_str().trim_end_matches('.').to_string();
        // ignore embedded image/asset "emails"
        if e.to_ascii_lowercase().ends_with(".png") || e.to_ascii_lowercase().ends_with(".jpg") {
            continue;
        }
        if !out.contains(&e) {
            out.push(e);
        }
    }
    out
}

/// All valid phone numbers (formatted E.164), assuming India when no country
/// code is present.
pub fn find_phones(text: &str) -> Vec<String> {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| Regex::new(r"\+?\(?\d[\d\s\-().]{6,}\d").unwrap());
    let mut out = Vec::new();
    for m in re.find_iter(text) {
        if let Some(e164) = normalize_phone(m.as_str()) {
            if !out.contains(&e164) {
                out.push(e164);
            }
        }
    }
    out
}

/// Normalize a raw phone token to validated E.164, defaulting to India (+91)
/// when no country code is present. libphonenumber mis-handles a bare 10-digit
/// Indian mobile, so we reconstruct the +91 form ourselves before validating.
pub fn normalize_phone(cand: &str) -> Option<String> {
    let has_plus = cand.trim_start().starts_with('+');
    let digits: String = cand.chars().filter(|c| c.is_ascii_digit()).collect();

    let e164_try = if has_plus {
        format!("+{digits}")
    } else if digits.len() == 10 && matches!(digits.as_bytes()[0], b'6'..=b'9') {
        format!("+91{digits}")
    } else if digits.len() == 11 && digits.starts_with('0') {
        format!("+91{}", &digits[1..])
    } else if digits.len() == 12 && digits.starts_with("91") {
        format!("+{digits}")
    } else {
        // Unknown shape — let libphonenumber try with the India region.
        return phonenumber::parse(Some(phonenumber::country::Id::IN), cand)
            .ok()
            .filter(phonenumber::is_valid)
            .map(|n| n.format().mode(Mode::E164).to_string());
    };

    phonenumber::parse(None, &e164_try)
        .ok()
        .filter(phonenumber::is_valid)
        .map(|n| n.format().mode(Mode::E164).to_string())
}

/// Does this e-mail appear verbatim in the source (case-insensitive)?
pub fn contains_email(source: &str, email: &str) -> bool {
    !email.is_empty() && source.to_ascii_lowercase().contains(&email.to_ascii_lowercase())
}

/// Does this phone (its national 10-digit tail) appear in the source's digits?
pub fn contains_phone(source: &str, e164: &str) -> bool {
    let digits: String = e164.chars().filter(|c| c.is_ascii_digit()).collect();
    if digits.len() < 7 {
        return false;
    }
    let tail: String = digits.chars().rev().take(10).collect::<String>().chars().rev().collect();
    let src_digits: String = source.chars().filter(|c| c.is_ascii_digit()).collect();
    src_digits.contains(&tail)
}

/// Whether an evidence snippet is actually supported by the source text:
/// normalized substring match, or ≥60% of its meaningful tokens present.
pub fn evidence_supported(snippet: &str, source: &str) -> bool {
    let s = normalize_ws(snippet);
    if s.is_empty() {
        return false;
    }
    let src = normalize_ws(source);
    if src.contains(&s) {
        return true;
    }
    let toks: Vec<&str> = s.split_whitespace().filter(|t| t.chars().count() >= 3).collect();
    if toks.is_empty() {
        return false;
    }
    let hits = toks.iter().filter(|t| src.contains(*t)).count();
    (hits as f64) / (toks.len() as f64) >= 0.6
}

fn normalize_ws(s: &str) -> String {
    s.to_ascii_lowercase().split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Deterministic role classifier used as a fallback when the Role agent fails or
/// returns nothing. Scores keyword hits per role against the resume text and
/// returns the best match with its score (0 ⇒ no signal, caller keeps Other).
pub fn classify_role_by_keywords(text: &str) -> (crate::config::Role, u32) {
    use crate::config::Role;
    let hay = text.to_ascii_lowercase();
    let mut best = Role::Other;
    let mut best_score = 0u32;
    for role in Role::ALL {
        if role == Role::Other {
            continue;
        }
        let mut score = 0u32;
        for kw in role.keywords() {
            // Cap per-keyword so one repeated word can't dominate.
            score += hay.matches(kw).count().min(3) as u32;
        }
        if score > best_score {
            best_score = score;
            best = role;
        }
    }
    (best, best_score)
}

// ---------------------------------------------------------------------------
// Temporal engine
// ---------------------------------------------------------------------------

/// Employment facts derived purely from parsed dates.
#[derive(Debug, Clone)]
pub struct Derived {
    pub currently_employed: bool,
    pub current_company: Option<String>,
    pub years_experience: f64,
    pub experience_level: ExperienceLevel,
    pub has_gap: bool,
    pub gap_months: i64,
    pub gap_duration: String,
    /// Diagnostics (unparseable dates etc.) that should trigger human review.
    pub notes: Vec<String>,
}

/// True when an end token means "still employed here".
pub fn is_present_token(s: &str) -> bool {
    let n = s.trim().to_ascii_lowercase();
    if n.is_empty() {
        return false;
    }
    ["present", "current", "now", "till date", "to date", "ongoing", "till now", "date"]
        .iter()
        .any(|t| n.contains(t))
}

fn month_from_str(s: &str) -> Option<u32> {
    let m = s.trim().to_ascii_lowercase();
    let names = [
        "jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec",
    ];
    names.iter().position(|n| m.starts_with(n)).map(|i| i as u32 + 1)
}

/// Parse a resume-style date. `prefer_end` makes a bare year resolve to December
/// (so a 2016–2019 range spans the whole of 2019).
pub fn parse_date(s: &str, prefer_end: bool) -> Option<NaiveDate> {
    let t = s.trim();
    if t.is_empty() {
        return None;
    }

    // "Jan 2020" / "January, 2020" / "Sep-2019"
    static RE_MON_YEAR: OnceLock<Regex> = OnceLock::new();
    let re_my = RE_MON_YEAR
        .get_or_init(|| Regex::new(r"(?i)([A-Za-z]{3,9})[.\-,/ ]+(\d{4})").unwrap());
    if let Some(c) = re_my.captures(t) {
        if let (Some(m), Ok(y)) = (month_from_str(&c[1]), c[2].parse::<i32>()) {
            return NaiveDate::from_ymd_opt(y, m, 1);
        }
    }

    // "03/2020" / "3-2020"
    static RE_MM_YYYY: OnceLock<Regex> = OnceLock::new();
    let re_mm = RE_MM_YYYY.get_or_init(|| Regex::new(r"^(\d{1,2})[/\-.](\d{4})$").unwrap());
    if let Some(c) = re_mm.captures(t) {
        if let (Ok(m), Ok(y)) = (c[1].parse::<u32>(), c[2].parse::<i32>()) {
            if (1..=12).contains(&m) {
                return NaiveDate::from_ymd_opt(y, m, 1);
            }
        }
    }

    // "2020-03" / "2020/03"
    static RE_YYYY_MM: OnceLock<Regex> = OnceLock::new();
    let re_ym = RE_YYYY_MM.get_or_init(|| Regex::new(r"^(\d{4})[/\-.](\d{1,2})$").unwrap());
    if let Some(c) = re_ym.captures(t) {
        if let (Ok(y), Ok(m)) = (c[1].parse::<i32>(), c[2].parse::<u32>()) {
            if (1..=12).contains(&m) {
                return NaiveDate::from_ymd_opt(y, m, 1);
            }
        }
    }

    // bare year "2020"
    static RE_YEAR: OnceLock<Regex> = OnceLock::new();
    let re_y = RE_YEAR.get_or_init(|| Regex::new(r"(\d{4})").unwrap());
    if let Some(c) = re_y.captures(t) {
        if let Ok(y) = c[1].parse::<i32>() {
            if (1950..=2100).contains(&y) {
                let m = if prefer_end { 12 } else { 1 };
                return NaiveDate::from_ymd_opt(y, m, 1);
            }
        }
    }
    None
}

/// Convert raw agent job entries into normalized jobs with parsed dates.
pub fn normalize_jobs(raws: &[JobRaw]) -> Vec<Job> {
    raws.iter()
        .map(|r| {
            let end_str = r.end.clone().unwrap_or_default();
            let present = is_present_token(&end_str);
            // Parse any concrete end date up front.
            let parsed_end = if present {
                None
            } else {
                r.end.as_deref().and_then(|s| parse_date(s, true))
            };
            // Trust the DATE over the model's is_current flag: an explicit end
            // date (even a recent one) means the role is NOT ongoing. Only treat
            // as current when the resume literally says Present/Current, or there
            // is no end date at all.
            let has_concrete_end = parsed_end.is_some();
            let is_current = present
                || (!has_concrete_end
                    && r.start.is_some()
                    && (r.is_current.unwrap_or(false) || r.end.is_none()));

            let start = r.start.as_deref().and_then(|s| parse_date(s, false));
            let end = if is_current { None } else { parsed_end };

            let title = r.title.clone().unwrap_or_default().trim().to_string();
            let mut et = r
                .employment_type
                .as_deref()
                .map(EmploymentType::from_str_loose)
                .unwrap_or(EmploymentType::FullTime);
            // A title containing "intern" is an internship regardless of what the
            // model labeled the type (but don't misread "internal"/"international").
            let tl = title.to_ascii_lowercase();
            if tl.contains("intern") && !tl.contains("internal") && !tl.contains("internation") {
                et = EmploymentType::Internship;
            }

            Job {
                company: r.company.clone().unwrap_or_default().trim().to_string(),
                title,
                start,
                end,
                is_current,
                employment_type: et,
                evidence: r.evidence.clone().unwrap_or_default(),
            }
        })
        .collect()
}

fn months_between(a: NaiveDate, b: NaiveDate) -> i64 {
    (b.year() as i64 - a.year() as i64) * 12 + (b.month() as i64 - a.month() as i64)
}

/// Merge overlapping intervals and sum their length in months (inclusive).
fn merge_sum_months(mut ivals: Vec<(NaiveDate, NaiveDate)>) -> i64 {
    if ivals.is_empty() {
        return 0;
    }
    ivals.sort_by_key(|(s, _)| *s);
    let mut total = 0i64;
    let (mut cs, mut ce) = ivals[0];
    for (s, e) in ivals.into_iter().skip(1) {
        if s <= ce {
            if e > ce {
                ce = e;
            }
        } else {
            total += months_between(cs, ce) + 1;
            cs = s;
            ce = e;
        }
    }
    total += months_between(cs, ce) + 1;
    total.max(0)
}

/// Derive employment facts from normalized jobs as of `today`.
pub fn derive_employment(jobs: &[Job], today: NaiveDate, gap_flag_months: i64) -> Derived {
    let mut notes = Vec::new();

    let prof: Vec<&Job> = jobs.iter().filter(|j| j.employment_type.is_professional()).collect();
    let experience_level = if prof.is_empty() {
        ExperienceLevel::Fresher
    } else {
        ExperienceLevel::Experienced
    };

    // Current employment = an ongoing professional role; pick the latest-started.
    let current_prof: Vec<&&Job> = prof.iter().filter(|j| j.is_current).collect();
    let currently_employed = !current_prof.is_empty();
    let current_company = current_prof
        .iter()
        .max_by_key(|j| j.start.unwrap_or(NaiveDate::MIN))
        .map(|j| j.company.clone())
        .filter(|c| !c.is_empty());

    // Build professional intervals (resolving ongoing → today) and track the
    // latest *completed* end for gap calculation.
    let mut intervals: Vec<(NaiveDate, NaiveDate)> = Vec::new();
    let mut latest_end: Option<NaiveDate> = None;
    for j in &prof {
        let end = if j.is_current { Some(today) } else { j.end };
        match (j.start, end) {
            (Some(s), Some(e)) if e >= s => {
                intervals.push((s, e));
                if !j.is_current {
                    latest_end = Some(latest_end.map_or(e, |le| le.max(e)));
                }
            }
            (Some(s), Some(_)) => {
                // e < s (the e >= s arm matched first)
                notes.push(format!("job end before start ({}): {}", j.company, j.title));
                intervals.push((s, s));
            }
            (None, _) => notes.push(format!("unparseable start date for {}", j.company)),
            (Some(_), None) => notes.push(format!("missing end date for {}", j.company)),
        }
    }

    let months = merge_sum_months(intervals);
    let years_experience = (months as f64 / 12.0 * 10.0).round() / 10.0;

    // Gap only when not currently employed.
    let (has_gap, gap_months) = if currently_employed {
        (false, 0)
    } else if let Some(end) = latest_end {
        let g = months_between(end, today).max(0);
        (g >= gap_flag_months, g)
    } else {
        (false, 0)
    };
    let gap_duration = if has_gap {
        humanize_months(gap_months)
    } else {
        "—".to_string()
    };

    Derived {
        currently_employed,
        current_company,
        years_experience,
        experience_level,
        has_gap,
        gap_months,
        gap_duration,
        notes,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::JobRaw;

    fn today() -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, 7, 19).unwrap()
    }

    fn raw(company: &str, title: &str, s: &str, e: &str, ty: &str) -> JobRaw {
        JobRaw {
            company: Some(company.into()),
            title: Some(title.into()),
            start: Some(s.into()),
            end: Some(e.into()),
            is_current: None,
            employment_type: Some(ty.into()),
            evidence: None,
        }
    }

    #[test]
    fn bob_employed_backend() {
        let jobs = normalize_jobs(&[
            raw("Acme Corp", "Senior Backend Engineer", "Jan 2021", "Present", "full-time"),
            raw("Beta Solutions", "Backend Developer", "June 2018", "December 2020", "full-time"),
        ]);
        let d = derive_employment(&jobs, today(), 2);
        assert!(d.currently_employed);
        assert_eq!(d.current_company.as_deref(), Some("Acme Corp"));
        assert!(!d.has_gap);
        assert_eq!(d.experience_level, ExperienceLevel::Experienced);
        // Jun2018–Dec2020 (31) + Jan2021–Jul2026 (67) ≈ 98 mo ≈ 8.2 yr
        assert!(d.years_experience > 7.5 && d.years_experience < 8.6, "yoe={}", d.years_experience);
    }

    #[test]
    fn charlie_has_current_gap() {
        // Regression: the model wrongly marks is_current=true, but the explicit
        // end date must win → not employed, real gap, ~6.4 yrs (not 7.4).
        let mut j = raw("Gamma Technologies", "Frontend Developer", "03/2019", "08/2025", "full-time");
        j.is_current = Some(true);
        let jobs = normalize_jobs(&[j]);
        assert!(!jobs[0].is_current, "concrete end date must override is_current");
        let d = derive_employment(&jobs, today(), 2);
        assert!(d.years_experience < 7.0, "yoe should be ~6.4, got {}", d.years_experience);
        assert!(!d.currently_employed);
        assert!(d.has_gap);
        // Aug 2025 → Jul 2026 ≈ 11 months
        assert!(d.gap_months >= 10 && d.gap_months <= 12, "gap={}", d.gap_months);
        assert_eq!(d.experience_level, ExperienceLevel::Experienced);
    }

    #[test]
    fn alice_fresher_intern_only() {
        let mut intern = raw("BrightMetrics", "Data Analyst Intern", "May 2024", "Jun 2024", "internship");
        intern.is_current = Some(false);
        let jobs = normalize_jobs(&[intern]);
        let d = derive_employment(&jobs, today(), 2);
        assert_eq!(d.experience_level, ExperienceLevel::Fresher);
        assert!(!d.currently_employed);
        assert!(!d.has_gap);
        assert_eq!(d.years_experience, 0.0);
    }

    #[test]
    fn phone_and_email_roundtrip() {
        let text = "Reach me at bob.sharma@outlook.com or 9123456789.";
        let emails = find_emails(text);
        assert_eq!(emails, vec!["bob.sharma@outlook.com"]);
        let phones = find_phones(text);
        assert_eq!(phones, vec!["+919123456789"]);
        assert!(contains_phone(text, "+919123456789"));
        assert!(contains_email(text, "bob.sharma@outlook.com"));
    }
}
