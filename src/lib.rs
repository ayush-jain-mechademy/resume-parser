//! Resume parser library: multi-agent, evidence-grounded extraction of
//! structured candidate data from resume files into a trustworthy spreadsheet.

pub mod agents;
pub mod config;
pub mod deterministic;
pub mod excel;
pub mod gemini;
pub mod ingest;
pub mod metrics;
pub mod pipeline;
pub mod schema;
pub mod store;
pub mod util;
