//! Data models: the raw shapes deserialized from each Gemini agent, the
//! normalized intermediate types, and the final `CandidateRecord` that is
//! persisted to the store and written to Excel.

use crate::config::{ExperienceLevel, Role};
use chrono::NaiveDate;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

// ---------------------------------------------------------------------------
// Confidence + row status
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Confidence {
    High,
    Medium,
    Low,
}

impl Confidence {
    pub fn label(&self) -> &'static str {
        match self {
            Confidence::High => "High",
            Confidence::Medium => "Medium",
            Confidence::Low => "Low",
        }
    }

    /// Higher rank = more confident. Used to take the weakest field as the
    /// overall confidence.
    pub fn rank(&self) -> u8 {
        match self {
            Confidence::Low => 0,
            Confidence::Medium => 1,
            Confidence::High => 2,
        }
    }

    /// Parse a model self-estimate string ("high"/"medium"/"low").
    pub fn from_str_loose(s: &str) -> Confidence {
        match s.trim().to_ascii_lowercase().as_str() {
            "high" => Confidence::High,
            "low" => Confidence::Low,
            _ => Confidence::Medium,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RowStatus {
    /// All fields high-confidence and internally consistent.
    AutoVerified,
    /// At least one field is uncertain or a sanity check failed.
    NeedsReview,
}

impl RowStatus {
    pub fn label(&self) -> &'static str {
        match self {
            RowStatus::AutoVerified => "✅ Verified",
            RowStatus::NeedsReview => "⚠️ Review",
        }
    }
}

// ---------------------------------------------------------------------------
// Raw agent outputs (deserialized straight from Gemini JSON)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default, Deserialize)]
pub struct ContactExtraction {
    pub name: Option<String>,
    pub name_evidence: Option<String>,
    pub email: Option<String>,
    pub phone: Option<String>,
    pub location: Option<String>,
    pub linkedin: Option<String>,
    pub github: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct JobRaw {
    pub company: Option<String>,
    pub title: Option<String>,
    /// Start date exactly as written in the resume (e.g. "Jan 2020", "03/2021").
    pub start: Option<String>,
    /// End date as written, or a "present"-like token if ongoing.
    pub end: Option<String>,
    pub is_current: Option<bool>,
    /// full-time | internship | freelance | contract | unknown
    pub employment_type: Option<String>,
    /// Verbatim line(s) from the resume backing this entry.
    pub evidence: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct EmploymentExtraction {
    #[serde(default)]
    pub jobs: Vec<JobRaw>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct RoleExtraction {
    pub primary_role: Option<String>,
    pub secondary_role: Option<String>,
    pub evidence: Option<String>,
    pub confidence: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct SkillsExtraction {
    #[serde(default)]
    pub skills: Vec<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct EducationRaw {
    pub degree: Option<String>,
    pub institution: Option<String>,
    pub start: Option<String>,
    pub end: Option<String>,
    pub evidence: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct EducationExtraction {
    #[serde(default)]
    pub education: Vec<EducationRaw>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct VerifierIssue {
    pub field: Option<String>,
    pub problem: Option<String>,
    /// high | medium | low
    pub severity: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct VerifierExtraction {
    #[serde(default)]
    pub issues: Vec<VerifierIssue>,
}

// ---------------------------------------------------------------------------
// Normalized intermediate types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum EmploymentType {
    FullTime,
    Internship,
    Freelance,
    Contract,
    Unknown,
}

impl EmploymentType {
    pub fn from_str_loose(s: &str) -> EmploymentType {
        let n = s.trim().to_ascii_lowercase();
        if n.contains("intern") {
            EmploymentType::Internship
        } else if n.contains("freelance") || n.contains("self") {
            EmploymentType::Freelance
        } else if n.contains("contract") || n.contains("consult") {
            EmploymentType::Contract
        } else if n.contains("full") || n.contains("permanent") || n.is_empty() {
            EmploymentType::FullTime
        } else {
            EmploymentType::Unknown
        }
    }

    /// Whether this counts toward "professional" experience and fresher/experienced.
    /// Only internships are excluded — a listed job whose type is unclear
    /// (Unknown) is still a real job and must count, else the model returning
    /// "unknown" would wrongly demote an experienced candidate to Fresher.
    pub fn is_professional(&self) -> bool {
        !matches!(self, EmploymentType::Internship)
    }
}

/// A normalized employment entry with parsed dates.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Job {
    pub company: String,
    pub title: String,
    pub start: Option<NaiveDate>,
    pub end: Option<NaiveDate>,
    pub is_current: bool,
    pub employment_type: EmploymentType,
    pub evidence: String,
}

// ---------------------------------------------------------------------------
// Final record
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CandidateRecord {
    pub source_file: String,
    pub file_hash: String,

    pub name: String,
    pub whatsapp: String,
    pub email: String,
    pub current_company: String,
    pub currently_employed: bool,
    pub has_gap: bool,
    pub gap_duration: String,
    pub gap_months: i64,
    pub years_experience: f64,
    pub experience_level: ExperienceLevel,
    pub primary_role: Role,
    pub secondary_role: Option<Role>,
    pub key_skills: Vec<String>,

    pub status: RowStatus,
    pub overall_confidence: Confidence,
    pub review_reasons: Vec<String>,
    /// Per-field confidence, keyed by column name.
    pub field_confidence: BTreeMap<String, Confidence>,
    /// Per-field verbatim evidence, keyed by column name.
    pub evidence: BTreeMap<String, String>,

    /// Full normalized work history, kept for audit + the review screen.
    pub jobs: Vec<Job>,
    /// True if the human has confirmed/corrected this row.
    #[serde(default)]
    pub human_verified: bool,
    /// True if extracted via the Gemini native-PDF vision fallback.
    #[serde(default)]
    pub used_vision: bool,
}

impl CandidateRecord {
    /// Recompute overall confidence + status from per-field confidence and any
    /// review reasons. Called after all fields are populated.
    pub fn finalize_status(&mut self) {
        let weakest = self
            .field_confidence
            .values()
            .map(|c| c.rank())
            .min()
            .unwrap_or(0);
        self.overall_confidence = match weakest {
            2 => Confidence::High,
            1 => Confidence::Medium,
            _ => Confidence::Low,
        };
        if self.human_verified {
            self.status = RowStatus::AutoVerified;
            self.overall_confidence = Confidence::High;
        } else if !self.review_reasons.is_empty() || weakest < 2 {
            self.status = RowStatus::NeedsReview;
        } else {
            self.status = RowStatus::AutoVerified;
        }
    }
}
