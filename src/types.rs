use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone)]
pub struct EngineContext {
    pub run_id: String,
    pub automation: Value,
    pub connection: Value,
    pub provider: Value,
    pub trigger: Value,
    pub client: reqwest::Client,
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
