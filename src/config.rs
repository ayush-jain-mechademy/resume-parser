//! Static configuration: the fixed IT role taxonomy, role synonyms/keywords used
//! by the deterministic cross-check, and tunable pipeline settings.

use serde::{Deserialize, Serialize};

/// The fixed set of IT role buckets a candidate's primary/secondary role must
/// map to. Keeping this a closed enum makes the spreadsheet column filterable
/// and lets us constrain Gemini's output to exactly these labels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Role {
    FullStack,
    Frontend,
    Backend,
    DataAnalyst,
    DataScientistMl,
    AiMlEngineer,
    DataEngineer,
    DevOpsCloud,
    Mobile,
    QaTesting,
    UiUx,
    BusinessSystemsAnalyst,
    ItSupport,
    Other,
}

impl Role {
    /// Every role variant, in display order.
    pub const ALL: [Role; 14] = [
        Role::FullStack,
        Role::Frontend,
        Role::Backend,
        Role::DataAnalyst,
        Role::DataScientistMl,
        Role::AiMlEngineer,
        Role::DataEngineer,
        Role::DevOpsCloud,
        Role::Mobile,
        Role::QaTesting,
        Role::UiUx,
        Role::BusinessSystemsAnalyst,
        Role::ItSupport,
        Role::Other,
    ];

    /// Human-readable label used in the Excel/JSON output and as the exact
    /// allowed value in Gemini's response schema.
    pub fn label(&self) -> &'static str {
        match self {
            Role::FullStack => "Full Stack",
            Role::Frontend => "Frontend",
            Role::Backend => "Backend",
            Role::DataAnalyst => "Data Analyst",
            Role::DataScientistMl => "Data Scientist/ML",
            Role::AiMlEngineer => "AI/ML Engineer",
            Role::DataEngineer => "Data Engineer",
            Role::DevOpsCloud => "DevOps/Cloud",
            Role::Mobile => "Mobile",
            Role::QaTesting => "QA/Testing",
            Role::UiUx => "UI/UX",
            Role::BusinessSystemsAnalyst => "Business/Systems Analyst",
            Role::ItSupport => "IT Support",
            Role::Other => "Other",
        }
    }

    /// The list of allowed labels for the schema `enum` constraint.
    pub fn all_labels() -> Vec<&'static str> {
        Role::ALL.iter().map(|r| r.label()).collect()
    }

    /// Parse a label (as returned by Gemini) back into the enum. Falls back to
    /// `Other` on anything unrecognized so a stray value never crashes the run.
    pub fn from_label(s: &str) -> Role {
        let norm = s.trim().to_ascii_lowercase();
        Role::ALL
            .iter()
            .copied()
            .find(|r| r.label().to_ascii_lowercase() == norm)
            .unwrap_or(Role::Other)
    }

    /// Keyword profile per role, used only as a deterministic cross-check /
    /// tie-breaker against the Role agent (never the sole source of truth).
    /// Lowercase, matched as word-ish substrings.
    pub fn keywords(&self) -> &'static [&'static str] {
        match self {
            Role::FullStack => &[
                "full stack", "full-stack", "mern", "mean", "fullstack",
                "front-end and back-end", "end to end",
            ],
            Role::Frontend => &[
                "frontend", "front-end", "front end", "react", "angular", "vue",
                "next.js", "nextjs", "tailwind", "css", "html", "redux", "ui developer",
            ],
            Role::Backend => &[
                "backend", "back-end", "back end", "node.js", "nodejs", "django",
                "spring", "spring boot", "flask", "express", "microservices", "rest api",
                "graphql", "golang", ".net", "laravel", "rails",
            ],
            Role::DataAnalyst => &[
                "data analyst", "power bi", "powerbi", "tableau", "excel", "dashboards",
                "sql", "reporting", "business intelligence", "looker", "data analysis",
            ],
            Role::DataScientistMl => &[
                "data scientist", "data science", "machine learning", "deep learning",
                "tensorflow", "pytorch", "nlp", "computer vision", "scikit", "model training",
                "statistical", "predictive model", "regression", "classification",
            ],
            Role::AiMlEngineer => &[
                "ai engineer", "gen ai", "genai", "generative ai", "llm", "large language model",
                "rag", "retrieval augmented", "langchain", "llamaindex", "agentic", "ai agent",
                "prompt engineering", "fine-tuning", "fine tuning", "mlops", "ml engineer",
                "hugging face", "vector database", "openai", "vertex ai", "bedrock",
            ],
            Role::DataEngineer => &[
                "data engineer", "etl", "spark", "hadoop", "airflow", "kafka",
                "data pipeline", "databricks", "snowflake", "dbt", "data warehouse",
            ],
            Role::DevOpsCloud => &[
                "devops", "sre", "kubernetes", "docker", "terraform", "ci/cd", "cicd",
                "aws", "azure", "gcp", "cloud engineer", "ansible", "jenkins", "helm",
                "platform engineer",
            ],
            Role::Mobile => &[
                "android", "ios", "flutter", "react native", "kotlin", "swift",
                "mobile developer", "mobile app", "jetpack compose",
            ],
            Role::QaTesting => &[
                "qa", "quality assurance", "test engineer", "sdet", "automation testing",
                "selenium", "cypress", "manual testing", "test cases", "appium",
            ],
            Role::UiUx => &[
                "ui/ux", "ux designer", "ui designer", "figma", "wireframe", "prototyping",
                "user research", "interaction design", "adobe xd",
            ],
            Role::BusinessSystemsAnalyst => &[
                "business analyst", "systems analyst", "requirements gathering",
                "stakeholder", "user stories", "brd", "functional specification",
            ],
            Role::ItSupport => &[
                "it support", "help desk", "helpdesk", "desktop support",
                "technical support", "system administrator", "sysadmin", "network support",
            ],
            Role::Other => &[],
        }
    }
}

/// Experience classification.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ExperienceLevel {
    Fresher,
    Experienced,
}

impl ExperienceLevel {
    pub fn label(&self) -> &'static str {
        match self {
            ExperienceLevel::Fresher => "Fresher",
            ExperienceLevel::Experienced => "Experienced",
        }
    }
}

/// Tunable pipeline settings (kept in one place for easy adjustment).
#[derive(Debug, Clone)]
pub struct Settings {
    /// Gemini model used by the specialist agents.
    pub model: String,
    /// Stronger model used by the Verifier's second opinion.
    pub verifier_model: String,
    /// Max concurrent resumes in flight.
    pub workers: usize,
    /// Global cap on simultaneous Gemini requests (free-tier friendly).
    pub max_concurrent: usize,
    /// Below this many extracted characters a PDF is treated as scanned and sent
    /// to Gemini vision instead of relying on local text.
    pub min_text_chars: usize,
    /// A current employment gap of at least this many months is flagged.
    pub gap_flag_months: i64,
    /// API call retry attempts on transient (429/503) errors.
    pub max_retries: u32,
    /// Hard wall-clock cap per resume. If a single file exceeds this (e.g. a
    /// pathological PDF stuck in the vision fallback), it is abandoned as a
    /// failure so it can never stall the batch.
    pub resume_timeout_secs: u64,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            model: "gemini-2.5-flash-lite".to_string(),
            verifier_model: "gemini-2.5-flash".to_string(),
            workers: 6,
            max_concurrent: 5,
            min_text_chars: 120,
            gap_flag_months: 2,
            max_retries: 4,
            resume_timeout_secs: 150,
        }
    }
}
