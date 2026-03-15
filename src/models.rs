use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Deserialize)]
pub struct SolveRequest {
    #[serde(default)]
    pub segment: String,
    #[serde(default)]
    pub fields_to_extract: Vec<String>,
    #[serde(default)]
    pub field_types: HashMap<String, String>,
    #[serde(default)]
    pub offers: Vec<OfferInput>,
    #[serde(default)]
    pub rfp: Option<RfpDocument>,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
pub struct OfferInput {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub insurer: String,
    #[serde(default)]
    pub label: String,
    #[serde(default)]
    pub documents: Vec<Document>,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
pub struct Document {
    #[serde(default)]
    pub filename: String,
    #[serde(default)]
    pub ocr_text: String,
    #[serde(default)]
    pub pdf_url: Option<String>,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
pub struct RfpDocument {
    #[serde(default)]
    pub filename: String,
    #[serde(default)]
    pub ocr_text: String,
    #[serde(default)]
    pub pdf_url: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct SolveResponse {
    pub offers_parsed: Vec<OfferParsed>,
    pub ranking: Vec<String>,
    pub best_offer_id: String,
}

#[derive(Debug, Serialize, Clone)]
pub struct OfferParsed {
    pub id: String,
    pub insurer: String,
    pub fields: HashMap<String, String>,
}

#[derive(Debug, Serialize)]
pub struct MetricsResponse {
    pub gemini_request_count: u64,
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub total_tokens: u64,
}

#[derive(Debug, Serialize)]
pub struct StatusResponse {
    pub status: String,
}
