use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Deserialize)]
pub struct SolveRequest {
    pub segment: String,
    pub fields_to_extract: Vec<String>,
    pub field_types: HashMap<String, String>,
    pub offers: Vec<OfferInput>,
    pub rfp: Option<RfpDocument>,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
pub struct OfferInput {
    pub id: String,
    pub insurer: String,
    pub label: String,
    pub documents: Vec<Document>,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
pub struct Document {
    pub filename: String,
    pub ocr_text: String,
    pub pdf_url: Option<String>,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
pub struct RfpDocument {
    pub filename: String,
    pub ocr_text: String,
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
