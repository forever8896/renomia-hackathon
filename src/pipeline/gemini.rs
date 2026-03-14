use std::collections::HashMap;
use std::sync::Arc;

use reqwest::Client;
use serde_json::{json, Value};
use tracing::{error, warn};

use crate::metrics::Metrics;

const GEMINI_MODEL: &str = "gemini-2.5-flash";
const MAX_RETRIES: u32 = 3;

pub struct GeminiClient {
    client: Client,
    api_key: String,
    metrics: Arc<Metrics>,
}

impl GeminiClient {
    pub fn new(client: Client, api_key: String, metrics: Arc<Metrics>) -> Self {
        Self {
            client,
            api_key,
            metrics,
        }
    }

    async fn call_gemini(&self, contents: Value, generation_config: Value) -> Result<Value, String> {
        let url = format!(
            "https://generativelanguage.googleapis.com/v1beta/models/{}:generateContent?key={}",
            GEMINI_MODEL, self.api_key
        );

        let body = json!({
            "contents": contents,
            "generationConfig": generation_config,
        });

        let mut last_err = String::new();
        for attempt in 0..MAX_RETRIES {
            if attempt > 0 {
                let delay = 1u64 << attempt; // 2s, 4s
                tokio::time::sleep(std::time::Duration::from_secs(delay)).await;
            }

            let resp = self.client.post(&url).json(&body).send().await;
            match resp {
                Ok(r) => {
                    let status = r.status();
                    let resp_text = r.text().await.unwrap_or_default();

                    if status == 429 || status == 503 {
                        warn!("Gemini {status}, attempt {attempt}, retrying...");
                        last_err = format!("HTTP {status}: {resp_text}");
                        continue;
                    }

                    if !status.is_success() {
                        last_err = format!("HTTP {status}: {resp_text}");
                        error!("Gemini error: {last_err}");
                        return Err(last_err);
                    }

                    let parsed: Value = serde_json::from_str(&resp_text)
                        .map_err(|e| format!("JSON parse error: {e}"))?;

                    // Track token usage
                    if let Some(usage) = parsed.get("usageMetadata") {
                        let prompt_tokens = usage.get("promptTokenCount")
                            .and_then(|v| v.as_u64()).unwrap_or(0);
                        let completion_tokens = usage.get("candidatesTokenCount")
                            .and_then(|v| v.as_u64()).unwrap_or(0);
                        self.metrics.add(prompt_tokens, completion_tokens);
                    }

                    return Ok(parsed);
                }
                Err(e) => {
                    last_err = format!("Request error: {e}");
                    warn!("Gemini request failed: {last_err}, attempt {attempt}");
                }
            }
        }

        Err(format!("All {MAX_RETRIES} retries failed: {last_err}"))
    }

    fn extract_text_from_response(parsed: &Value) -> Result<String, String> {
        parsed
            .get("candidates")
            .and_then(|c| c.get(0))
            .and_then(|c| c.get("content"))
            .and_then(|c| c.get("parts"))
            .and_then(|p| p.get(0))
            .and_then(|p| p.get("text"))
            .and_then(|t| t.as_str())
            .map(|s| s.to_string())
            .ok_or_else(|| "No text in Gemini response".to_string())
    }

    pub async fn extract_fields(
        &self,
        _offer_id: &str,
        insurer: &str,
        segment: &str,
        fields: &[String],
        field_types: &HashMap<String, String>,
        documents_text: &str,
        rfp_text: Option<&str>,
    ) -> HashMap<String, String> {
        let fields_list: String = fields
            .iter()
            .map(|f| {
                let ftype = field_types.get(f).map(|s| s.as_str()).unwrap_or("string");
                format!("- {} (type: {})", f, ftype)
            })
            .collect::<Vec<_>>()
            .join("\n");

        let mut prompt = format!(
            r#"You are an expert insurance document analyst specializing in Czech and international insurance markets.

TASK: Extract these fields from the insurance offer by "{insurer}" in the "{segment}" segment.

CRITICAL RULES:
1. For NUMBER type fields: return ONLY the plain numeric value as digits (e.g. "34851", "150000000", "5000"). Remove currency symbols, spaces, dots/commas used as thousands separators. Keep decimal points for fractional values (e.g. "459.35"). If the value contains a percentage or formula like "3 % / min. CZK 3 000", return it as-is.
2. For STRING type fields: return a concise value. For yes/no coverages, prefer "Ano" or "Ne" with brief qualifiers only when meaningful.
3. If a field is truly not found and cannot be inferred, return "N/A".
4. Search ALL text carefully — values may be in tables, lists, footnotes, summaries, or scattered sections. Documents may be in Czech, English, or German.
5. CRITICAL — Distinguish between PREMIUMS (what the client PAYS, annual cost) and COVERAGE LIMITS (maximum payout amounts):
   - "Roční pojistné", "roční pojistné", "CELKEM", "pojistné", "Premium", "Total premium" = amounts the client pays per year
   - "limit", "pojistná částka", "Sum insured", "Coverage limit" = maximum coverage amounts
   - These are very different numbers — a premium might be 15,000 while coverage is 10,000,000
6. Insurance domain knowledge:
   - "Allrisk" havarijní pojištění INCLUDES: odcizení, vandalismus, živelní rizika, střet se zvěří, skla — answer "Ano" unless explicitly excluded
   - "Limit X/Y mil" means X million — return as full number (200000000)
   - Spoluúčast with % and minimum → return "3 % / min. CZK 3 000"
   - "Cena celkem" or total annual = "Roční pojistné"
   - "Pojistná částka" = coverage limit, NOT premium
7. If a field is referenced but its value isn't stated, return "Neuvedeno".
8. The field names are in Czech but document text may be in English/German. Match semantically:
   - "Havarijní pojištění" (premium) = "Hull insurance premium", "Kaskoversicherung"
   - "Pojištění odpovědnosti" = "Liability insurance", "Third party liability"
   - "spoluúčast" = "deductible", "excess", "Selbstbeteiligung"
   - "pojistná částka" = "sum insured", "insured value"

Fields to extract:
{fields_list}

{documents_text}"#
        );

        if let Some(rfp) = rfp_text {
            prompt.push_str(&format!(
                "\n\n=== Request for Proposal (RFP) - for context only ===\n{}",
                rfp
            ));
        }

        // Build responseSchema
        let mut properties = serde_json::Map::new();
        let mut required = vec!["reasoning".to_string()];

        properties.insert(
            "reasoning".to_string(),
            json!({"type": "STRING", "description": "Brief reasoning about what you found"}),
        );

        for field in fields {
            properties.insert(field.clone(), json!({"type": "STRING"}));
            required.push(field.clone());
        }

        let response_schema = json!({
            "type": "OBJECT",
            "properties": properties,
            "required": required,
        });

        let generation_config = json!({
            "responseMimeType": "application/json",
            "responseSchema": response_schema,
            "temperature": 0.0,
        });

        let contents = json!([{
            "parts": [{"text": prompt}]
        }]);

        match self.call_gemini(contents, generation_config).await {
            Ok(parsed) => {
                match Self::extract_text_from_response(&parsed) {
                    Ok(text) => {
                        match serde_json::from_str::<HashMap<String, Value>>(&text) {
                            Ok(map) => {
                                let mut result = HashMap::new();
                                for field in fields {
                                    let val = map
                                        .get(field)
                                        .and_then(|v| match v {
                                            Value::String(s) if !s.is_empty() => Some(s.clone()),
                                            Value::Number(n) => Some(n.to_string()),
                                            _ => None,
                                        })
                                        .unwrap_or_else(|| "N/A".to_string());
                                    result.insert(field.clone(), val);
                                }
                                result
                            }
                            Err(e) => {
                                error!("Failed to parse extraction response: {e}, text: {text}");
                                fields.iter().map(|f| (f.clone(), "N/A".to_string())).collect()
                            }
                        }
                    }
                    Err(e) => {
                        error!("No text in extraction response: {e}");
                        fields.iter().map(|f| (f.clone(), "N/A".to_string())).collect()
                    }
                }
            }
            Err(e) => {
                error!("Gemini extraction failed: {e}");
                fields.iter().map(|f| (f.clone(), "N/A".to_string())).collect()
            }
        }
    }

    pub async fn rank_offers(
        &self,
        segment: &str,
        offers: &[(String, String, HashMap<String, String>)], // (id, insurer, fields)
        field_types: &HashMap<String, String>,
    ) -> Result<Vec<String>, String> {
        let mut offers_text = String::new();
        for (id, insurer, fields) in offers {
            offers_text.push_str(&format!("\n## Offer: {} ({})\n", insurer, id));
            for (field, value) in fields {
                let ftype = field_types.get(field).map(|s| s.as_str()).unwrap_or("string");
                offers_text.push_str(&format!("- {} [{}]: {}\n", field, ftype, value));
            }
        }

        let offer_ids: Vec<&str> = offers.iter().map(|(id, _, _)| id.as_str()).collect();
        let ids_json = serde_json::to_string(&offer_ids).unwrap();

        let prompt = format!(
            r#"You are comparing {n} insurance offers in the "{segment}" segment.
Rank them from best (most favorable to the client/buyer) to worst.

For ranking, consider:
- Lower annual premium (Roční pojistné) is better
- Higher coverage limits (limit, pojistná částka) are better
- Lower deductibles (spoluúčast) are better
- More included coverages (Ano > Ne) is better
- Coverage breadth and quality matters more than small price differences
- For the "{segment}" segment, weigh all factors holistically

The offer IDs are: {ids_json}

{offers_text}

Return a JSON array of offer IDs from best to worst. Use ONLY these exact IDs: {ids_json}"#,
            n = offers.len(),
        );

        let response_schema = json!({
            "type": "ARRAY",
            "items": {"type": "STRING"},
        });

        let generation_config = json!({
            "responseMimeType": "application/json",
            "responseSchema": response_schema,
            "temperature": 0.0,
        });

        let contents = json!([{
            "parts": [{"text": prompt}]
        }]);

        let parsed = self.call_gemini(contents, generation_config).await?;
        let text = Self::extract_text_from_response(&parsed)?;

        let ranking: Vec<String> = serde_json::from_str(&text)
            .map_err(|e| format!("Failed to parse ranking: {e}, text: {text}"))?;

        // Validate all IDs are present
        let valid_ids: std::collections::HashSet<&str> = offer_ids.iter().copied().collect();
        let mut seen = std::collections::HashSet::new();
        let mut validated = Vec::new();
        for id in &ranking {
            if valid_ids.contains(id.as_str()) && seen.insert(id.as_str()) {
                validated.push(id.clone());
            }
        }
        // Add any missing IDs at the end
        for id in &offer_ids {
            if !seen.contains(id) {
                validated.push(id.to_string());
            }
        }

        Ok(validated)
    }
}
