use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::Arc;

#[derive(Clone)]
pub struct EngineContext {
    pub run_id: String,
    pub automation: Value,
    pub connection: Value,
    pub provider: Value,
    pub trigger: Value,
    pub client: reqwest::Client,
    /// Tokens the providers reported while this run was in flight. Sharing the
    /// cell keeps spend from failed runs billable in the overview, where the
    /// money was really spent.
    pub usage: Arc<std::sync::Mutex<crate::providers::TokenUsage>>,
    /// Set when the plan refuses organization-owned repositories; holds the
    /// explanation that follows the repository name in the run error.
    pub organization_restriction: Option<&'static str>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct RunOutput {
    pub summary: String,
    #[serde(default)]
    pub findings: Vec<Finding>,
    #[serde(default)]
    pub logs: Vec<String>,
    #[serde(default)]
    pub artifacts: Value,
    #[serde(default)]
    pub usage: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Finding {
    pub severity: String,
    pub title: String,
    #[serde(default)]
    pub file: String,
    #[serde(default)]
    pub line: Option<u32>,
    pub explanation: String,
    pub suggestion: String,
}
