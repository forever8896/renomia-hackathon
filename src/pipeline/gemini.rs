use std::collections::HashMap;
use std::sync::Arc;

use reqwest::Client;
use serde_json::{json, Value};
use tracing::{error, warn};

use crate::metrics::Metrics;

const EXTRACT_MODEL: &str = "gemini-3.1-flash-lite-preview";
const MAX_RETRIES: u32 = 4;
const MAX_PDF_SIZE: usize = 20_000_000; // 20MB limit for Gemini file upload

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

    async fn call_gemini(
        &self,
        model: &str,
        contents: Value,
        generation_config: Value,
        system_instruction: Option<&str>,
    ) -> Result<Value, String> {
        let url = format!(
            "https://generativelanguage.googleapis.com/v1beta/models/{}:generateContent?key={}",
            model, self.api_key
        );

        let mut body = json!({
            "contents": contents,
            "generationConfig": generation_config,
        });

        if let Some(sys_text) = system_instruction {
            body.as_object_mut().unwrap().insert(
                "system_instruction".to_string(),
                json!({"parts": [{"text": sys_text}]}),
            );
        }

        let mut last_err = String::new();
        for attempt in 0..MAX_RETRIES {
            if attempt > 0 {
                let delay = 1u64 << attempt;
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
                        // Retry on 500s
                        if status.is_server_error() {
                            continue;
                        }
                        return Err(last_err);
                    }

                    let parsed: Value = serde_json::from_str(&resp_text)
                        .map_err(|e| format!("JSON parse error: {e}"))?;

                    if let Some(usage) = parsed.get("usageMetadata") {
                        let prompt_tokens = usage.get("promptTokenCount")
                            .and_then(|v| v.as_u64()).unwrap_or(0);
                        let completion_tokens = usage.get("candidatesTokenCount")
                            .and_then(|v| v.as_u64()).unwrap_or(0);
                        let thoughts_tokens = usage.get("thoughtsTokenCount")
                            .and_then(|v| v.as_u64()).unwrap_or(0);
                        self.metrics.add(prompt_tokens, completion_tokens + thoughts_tokens);
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

    /// Upload a document to Gemini File API, returns the file URI.
    /// Supports PDF and DOCX based on the provided MIME type.
    pub async fn upload_document(&self, bytes: &[u8], mime_type: &str) -> Result<String, String> {
        let url = format!(
            "https://generativelanguage.googleapis.com/upload/v1beta/files?key={}",
            self.api_key
        );

        let resp = self.client.post(&url)
            .header("X-Goog-Upload-Command", "start, upload, finalize")
            .header("X-Goog-Upload-Header-Content-Length", bytes.len().to_string())
            .header("X-Goog-Upload-Header-Content-Type", mime_type)
            .header("Content-Type", mime_type)
            .body(bytes.to_vec())
            .send()
            .await
            .map_err(|e| format!("Document upload failed: {e}"))?;

        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        if !status.is_success() {
            return Err(format!("PDF upload HTTP {status}: {text}"));
        }

        let parsed: Value = serde_json::from_str(&text)
            .map_err(|e| format!("PDF upload parse error: {e}"))?;

        parsed.get("file")
            .and_then(|f| f.get("uri"))
            .and_then(|u| u.as_str())
            .map(|s| s.to_string())
            .ok_or_else(|| format!("No URI in upload response: {text}"))
    }

    /// Create a context cache for reuse across multiple extraction calls on the same document.
    /// Returns the cache name (e.g., "cachedContents/abc123").
    pub async fn create_context_cache(
        &self,
        documents_text: &str,
        doc_uris: &[(String, String)],
    ) -> Result<String, String> {
        let url = format!(
            "https://generativelanguage.googleapis.com/v1beta/cachedContents?key={}",
            self.api_key
        );

        let system_instruction = Self::extraction_system_instruction();

        let mut parts = Vec::new();
        for (uri, mime) in doc_uris {
            parts.push(json!({"fileData": {"fileUri": uri, "mimeType": mime}}));
        }
        parts.push(json!({"text": documents_text}));

        let body = json!({
            "model": format!("models/{}", EXTRACT_MODEL),
            "contents": [{"role": "user", "parts": parts}],
            "systemInstruction": {"parts": [{"text": system_instruction}]},
            "ttl": "600s"
        });

        let resp = self.client.post(&url)
            .json(&body)
            .send()
            .await
            .map_err(|e| format!("Cache creation failed: {e}"))?;

        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        if !status.is_success() {
            return Err(format!("Cache creation HTTP {status}: {}", &text[..text.len().min(200)]));
        }

        let parsed: Value = serde_json::from_str(&text)
            .map_err(|e| format!("Cache parse error: {e}"))?;

        parsed.get("name")
            .and_then(|n| n.as_str())
            .map(|s| s.to_string())
            .ok_or_else(|| format!("No name in cache response"))
    }

    /// Call Gemini using a cached context (for batched extraction on same document).
    async fn call_gemini_cached(
        &self,
        cache_name: &str,
        contents: Value,
        generation_config: Value,
    ) -> Result<Value, String> {
        let url = format!(
            "https://generativelanguage.googleapis.com/v1beta/models/{}:generateContent?key={}",
            EXTRACT_MODEL, self.api_key
        );

        let body = json!({
            "cachedContent": cache_name,
            "contents": contents,
            "generationConfig": generation_config,
        });

        let mut last_err = String::new();
        for attempt in 0..MAX_RETRIES {
            if attempt > 0 {
                let delay = 1u64 << attempt;
                tokio::time::sleep(std::time::Duration::from_secs(delay)).await;
            }

            let resp = self.client.post(&url).json(&body).send().await;
            match resp {
                Ok(r) => {
                    let status = r.status();
                    let resp_text = r.text().await.unwrap_or_default();

                    if status == 429 || status == 503 {
                        warn!("Gemini cached {status}, attempt {attempt}, retrying...");
                        last_err = format!("HTTP {status}");
                        continue;
                    }

                    if !status.is_success() {
                        last_err = format!("HTTP {status}: {}", &resp_text[..resp_text.len().min(300)]);
                        error!("Gemini cached error: {last_err}");
                        if status.is_server_error() { continue; }
                        return Err(last_err);
                    }

                    let parsed: Value = serde_json::from_str(&resp_text)
                        .map_err(|e| format!("JSON parse error: {e}"))?;

                    if let Some(usage) = parsed.get("usageMetadata") {
                        let prompt_tokens = usage.get("promptTokenCount")
                            .and_then(|v| v.as_u64()).unwrap_or(0);
                        let completion_tokens = usage.get("candidatesTokenCount")
                            .and_then(|v| v.as_u64()).unwrap_or(0);
                        let thoughts_tokens = usage.get("thoughtsTokenCount")
                            .and_then(|v| v.as_u64()).unwrap_or(0);
                        self.metrics.add(prompt_tokens, completion_tokens + thoughts_tokens);
                    }

                    return Ok(parsed);
                }
                Err(e) => {
                    last_err = format!("Request error: {e}");
                    warn!("Gemini cached request failed: {last_err}, attempt {attempt}");
                }
            }
        }

        Err(format!("All {MAX_RETRIES} retries failed: {last_err}"))
    }

    /// Download a PDF from a URL, returns the bytes.
    pub async fn download_pdf(&self, url: &str) -> Result<Vec<u8>, String> {
        let resp = self.client.get(url)
            .send()
            .await
            .map_err(|e| format!("PDF download failed: {e}"))?;

        if !resp.status().is_success() {
            return Err(format!("PDF download HTTP {}", resp.status()));
        }

        let bytes = resp.bytes().await
            .map_err(|e| format!("PDF read failed: {e}"))?;

        if bytes.len() > MAX_PDF_SIZE {
            return Err(format!("PDF too large: {} bytes", bytes.len()));
        }

        Ok(bytes.to_vec())
    }

    fn extract_text_from_response(parsed: &Value) -> Result<String, String> {
        // With thinking enabled, there may be multiple parts — find the one with text (not thought)
        let parts = parsed
            .get("candidates")
            .and_then(|c| c.get(0))
            .and_then(|c| c.get("content"))
            .and_then(|c| c.get("parts"))
            .and_then(|p| p.as_array())
            .ok_or_else(|| "No parts in Gemini response".to_string())?;

        // Return the last text part that is not a thought
        for part in parts.iter().rev() {
            if part.get("thought").and_then(|t| t.as_bool()).unwrap_or(false) {
                continue;
            }
            if let Some(text) = part.get("text").and_then(|t| t.as_str()) {
                return Ok(text.to_string());
            }
        }

        Err("No text in Gemini response".to_string())
    }

    /// Generate a smart description for a field based on its name and type.
    fn field_description(field: &str, ftype: &str) -> String {
        let fl = field.to_lowercase();

        if ftype == "number" {
            if fl.contains("roční pojistn") || fl.contains("celkem") || fl == "roční pojistné:" {
                return "Annual premium as plain number (e.g. 34851)".to_string();
            }
            if fl.contains("spoluúčast") {
                if fl.contains("skla") || fl.contains("skel") {
                    return "Glass deductible - if % and min, return as formula e.g. '3 % / min. CZK 3 000'".to_string();
                }
                return "Deductible as plain number (e.g. 5000), or formula if % (e.g. '3 % / min. CZK 3 000')".to_string();
            }
            if fl.contains("limit") {
                return "Coverage limit as plain number (e.g. 50000000 for 50 mil)".to_string();
            }
            if fl.contains("částk") {
                return "Sum insured as plain number".to_string();
            }
            if fl.contains("sleva") {
                return "Discount amount as plain number".to_string();
            }
            if fl.contains("pojistn") && !fl.contains("limit") {
                return "Premium/cost as plain number".to_string();
            }
            return "Numeric value as plain digits".to_string();
        }

        // String fields
        if fl.contains("krytí") || fl.contains("pojištění") && !fl.contains("limit") && !fl.contains("spoluúčast") {
            return "Coverage: 'Ano' / 'Ano, se spoluúčastí CZK X' / 'Ne (lze připojistit za CZK X)' / 'Ne, pouze připojištění'".to_string();
        }
        if fl.contains("typ") && fl.contains("havarijní") {
            return "'Allrisk' or 'Allrisk + GAP' — no variant names".to_string();
        }
        if fl.contains("rozsah") && fl.contains("servis") {
            return "'Volba servisu' / 'Povinné smluvní servisy' / 'Volba servisu, sleva CZK X'".to_string();
        }
        if fl.contains("asistenční") && fl.contains("rozsah") {
            return "Concise: 'Rozšířená asistence' / '30 min / 50–100 km' / 'CZK 2 500 / CZK 5 000'".to_string();
        }
        if fl.contains("počet") && fl.contains("asisten") {
            return "'Neomezeno' or 'Omezeno' — no detailed descriptions".to_string();
        }
        if fl.contains("přímá likvidace") {
            return "'Ano' or 'Ne (lze připojistit)'".to_string();
        }
        if fl.contains("právní ochrana") {
            return "'Ano' / 'Ano (právní poradenství)' / 'Ne (lze připojistit za CZK 1 485)'".to_string();
        }
        if fl.contains("úrazové") {
            return "'Ano' / 'Ano (jen řidič)' / 'Volitelné připojištění' / 'Ne (lze připojistit)'".to_string();
        }
        if fl.contains("územní rozsah") {
            return "Territory list: 'Česká republika, Slovensko, Polsko'".to_string();
        }
        if fl.contains("vyloučen") {
            return "Concise summary of exclusions".to_string();
        }
        // For odpovědnost-style fields: when the field name refers to a coverage type
        // (e.g. "Věci zaměstnanců", "Regresní náhrady", "Smluvní pokuty"),
        // return the COVERAGE LIMIT as the value (e.g. "CZK 50,000,000"),
        // or "Vyloučeno"/"Vynecháno" if not covered, not just "Ano"/"Ne"
        if fl.contains("limit") || fl.contains("sublimit") {
            return "Coverage limit value, e.g. 'CZK 50,000,000' or 'Vyloučeno'".to_string();
        }
        "Return what the document states: specific limit (CZK X) if stated, 'Ano'/'Ne' if only coverage status, with qualifiers when relevant.".to_string()
    }

    fn extraction_system_instruction() -> String {
        r#"You are an expert insurance document analyst specializing in Czech and international insurance markets.

CRITICAL RULES:
1. For NUMBER type fields: return ONLY the plain numeric value as digits (e.g. "34851", "150000000", "5000"). Remove currency symbols (Kč, CZK, EUR, €), spaces, dots/commas used as thousands separators. Keep decimal points for fractional values (e.g. "459.35"). If the value contains a percentage or formula like "3 % / min. CZK 3 000", return it as-is since it's not a pure number.
2. For STRING type fields — follow these format rules EXACTLY:
   a) For coverage yes/no: Include qualifying details (deductible, price, condition) when present in the document.
      GOOD: "Ano, se spoluúčastí CZK 1,000" | "Ne (lze připojistit za CZK 1 485)" | "Ano (jen řidič)" | "Ne, pouze připojištění" | "Volitelné připojištění"
      BAD: just "Ano" or "Ne" when the doc has more detail
   b) For ranges/limits: use concise format with "–" for ranges: "CZK 10,000–50,000" or "30 min / 50–100 km"
   c) For Typ havarijního pojištění: use "Allrisk" (not variant names). If GAP is also included, use "Allrisk + GAP"
   d) For Rozsah servisu: prefer terms like "Volba servisu" or "Povinné smluvní servisy" over brand names
   e) For Asistenční služby – rozsah: use concise format like "Rozšířená asistence" or "30 min / 50–100 km" or "CZK 2 500 / CZK 5 000" (limits)
   f) For Počet zásahů asistence: use "Neomezeno" or "Omezeno" — not detailed descriptions
   g) Do NOT add product variant names, marketing labels, or article references: "Allrisk" not "Allrisk (Varianta Max)", "Ano" not "Ano (dle RGL4)"
   h) When coverage is optional/add-on, ALWAYS include the price: "Ne (lze připojistit za CZK 1 485)" not just "Ne (lze připojistit)"
   i) Přímá likvidace: answer "Ano" or "Ne (lze připojistit)" — not brand names
   k) For LIABILITY fields (odpovědnost segment): when a string field refers to a coverage type
      (e.g. "Věci zaměstnanců", "Regresní náhrady", "Smluvní pokuty", "Krytí vadného výrobku"),
      return what the document specifies:
      - If a specific LIMIT is stated → return "CZK 50,000,000" or "50 000 000 Kč"
      - If two limits (I/II) → return the specific variant
      - If ranges → "CZK 50,000,000–100,000,000"
      - If only confirmed as covered without a specific sublimit → "Ano"
      - If excluded → "Vyloučeno" or "Vynecháno"
      - If not mentioned → "Neuvedeno"
      Do NOT invent limits from the main policy limit — only use sublimit values explicitly stated for that specific coverage.
   j) Úrazové pojištění: if included for driver only, use "Ano (jen řidič)"
3. AVOID returning "N/A" — try harder. Look for synonyms, related terms, implied values. If a field is referenced but the value isn't stated, return "Neuvedeno". Only use "N/A" as absolute last resort.
   For Allrisk packages: check the SPECIFIC offer document for what's included vs optional:
   - If the document lists a coverage as included or part of the package → "Ano" (with qualifier if relevant)
   - If listed as optional add-on with a price → "Ne (lze připojistit za CZK X)"
   - If listed as only an add-on without details → "Ne, pouze připojištění"
   - Přímá likvidace / pojištění nezaviněné nehody are typically included → "Ano"
   - Úrazové pojištění for driver is often included → "Ano (jen řidič)"
4. Search ALL text carefully — values may be in tables, lists, footnotes, summaries, appendices, or scattered across sections. Documents may be in Czech, English, or German.
5. MOST CRITICAL — PREMIUMS vs COVERAGE LIMITS:
   When the field list contains cost-related fields together (pojistné, CELKEM, Sleva, pojistné před slevou), then bare insurance product names as fields refer to the PREMIUM (cost) for that product, NOT the coverage limit:
   - "Havarijní pojištění" = the PREMIUM for hull insurance (e.g. 370 EUR, 12456 CZK), NOT the insured value
   - "Pojištění odpovědnosti za škodu" = the PREMIUM for liability insurance, NOT the liability limit
   - "roční pojistné" = annual premium for a specific coverage line, NOT the total
   - "CELKEM" / "Total" = total annual premium across all coverages
   - "Sleva" = discount amount
   Only fields with "limit", "pojistná částka", "částka v případě" in the name refer to coverage limits/sums insured.
   Premiums are typically small numbers (hundreds to tens of thousands). Coverage limits are large (millions).
6. Insurance domain knowledge:
   - "Allrisk" havarijní pojištění INCLUDES: odcizení, vandalismus, živelní rizika, střet se zvěří, skla — all "Ano"
   - "Limit X/Y mil" = X million per event — return full number (e.g. "200/200 mil" → 200000000)
   - Spoluúčast with both % and minimum → return as formula "3 % / min. CZK 3 000"
   - "Pojistná částka" for vehicle = "Havarijní pojištění – limit"
   - If a coverage is listed as optional add-on → "Ne (lze připojistit)"
   - If a coverage is explicitly excluded → "Vyloučeno"
7. Multi-language matching (field names are Czech, docs may be English/German):
   - "Havarijní pojištění" (premium) = "Hull insurance premium" / "Kaskoversicherungsprämie"
   - "Pojištění odpovědnosti" = "Liability insurance premium" / "Third party liability premium"
   - "spoluúčast" = "deductible" / "excess" / "Selbstbeteiligung"
   - "CELKEM" = "Total" / "Gesamt" / "Total premium"
   - "roční pojistné" = "annual premium" / "Jahresprämie"
   - "pojistná částka" / "Sum insured" = coverage limit (NOT premium)
   - "Pojištěná částka v případě úmrtí" = "Sum insured in case of death""#.to_string()
    }

    fn build_extraction_prompt(
        insurer: &str,
        segment: &str,
        fields_list: &str,
        documents_text: &str,
        rfp_text: Option<&str>,
    ) -> String {
        // List field names in prompt as a search checklist (not the schema structure).
        // The schema handles format; the prompt tells the model WHAT to search for.
        let mut prompt = format!(
            r#"TASK: Extract the requested fields from this insurance offer by "{insurer}" in the "{segment}" segment.

Search the ENTIRE document carefully for each of these fields:
{fields_list}

{documents_text}"#
        );

        if let Some(rfp) = rfp_text {
            prompt.push_str(&format!(
                "\n\n=== Request for Proposal (RFP) - use for context ===\n{}",
                rfp
            ));
        }

        prompt
    }

    fn parse_extraction_response(text: &str, fields: &[String]) -> HashMap<String, String> {
        match serde_json::from_str::<HashMap<String, Value>>(text) {
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
                error!("Failed to parse extraction response: {e}, text: {}", &text[..text.len().min(500)]);
                fields.iter().map(|f| (f.clone(), "N/A".to_string())).collect()
            }
        }
    }

    async fn extract_fields_single_batch(
        &self,
        insurer: &str,
        segment: &str,
        fields: &[String],
        field_types: &HashMap<String, String>,
        documents_text: &str,
        rfp_text: Option<&str>,
        doc_uris: &[(String, String)], // Gemini file URIs for uploaded PDFs
    ) -> HashMap<String, String> {
        let fields_list: String = fields
            .iter()
            .map(|f| {
                let ftype = field_types.get(f).map(|s| s.as_str()).unwrap_or("string");
                format!("- {} (type: {})", f, ftype)
            })
            .collect::<Vec<_>>()
            .join("\n");

        let prompt = Self::build_extraction_prompt(insurer, segment, &fields_list, documents_text, rfp_text);

        let mut properties = serde_json::Map::new();
        let mut required = vec!["reasoning".to_string()];
        let mut property_ordering = vec!["reasoning".to_string()];
        properties.insert(
            "reasoning".to_string(),
            json!({"type": "STRING", "description": "Brief reasoning about what you found"}),
        );
        for field in fields {
            let ftype = field_types.get(field).map(|s| s.as_str()).unwrap_or("string");
            let description = Self::field_description(field, ftype);
            properties.insert(field.clone(), json!({"type": "STRING", "description": description}));
            required.push(field.clone());
            property_ordering.push(field.clone());
        }

        let response_schema = json!({
            "type": "OBJECT",
            "properties": properties,
            "required": required,
            "propertyOrdering": property_ordering,
        });

        // Scale thinking budget with document size — more text needs more reasoning
        let thinking_budget = if documents_text.len() > 100_000 { 8192 }
            else if documents_text.len() > 30_000 { 4096 }
            else { 2048 };

        let generation_config = json!({
            "responseMimeType": "application/json",
            "responseSchema": response_schema,
            "temperature": 0.0,
            "topP": 1.0,
            "seed": 42,
            "thinkingConfig": {"thinkingBudget": thinking_budget},
        });

        // Build parts: documents first, then text prompt
        let mut parts = Vec::new();
        for (uri, mime) in doc_uris {
            parts.push(json!({"fileData": {"fileUri": uri, "mimeType": mime}}));
        }
        parts.push(json!({"text": prompt}));

        let contents = json!([{"parts": parts}]);

        let system_instruction = Self::extraction_system_instruction();

        match self.call_gemini(EXTRACT_MODEL, contents, generation_config, Some(&system_instruction)).await {
            Ok(parsed) => {
                match Self::extract_text_from_response(&parsed) {
                    Ok(text) => Self::parse_extraction_response(&text, fields),
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

    /// Extract fields using a cached context (document text already stored in cache).
    async fn extract_fields_cached(
        &self,
        cache_name: &str,
        insurer: &str,
        segment: &str,
        fields: &[String],
        field_types: &HashMap<String, String>,
    ) -> HashMap<String, String> {
        let fields_list: String = fields
            .iter()
            .map(|f| {
                let ftype = field_types.get(f).map(|s| s.as_str()).unwrap_or("string");
                format!("- {} (type: {})", f, ftype)
            })
            .collect::<Vec<_>>()
            .join("\n");

        // Only send the task prompt — document is in cache
        let prompt = format!(
            "TASK: Extract these fields from the insurance offer by \"{insurer}\" in the \"{segment}\" segment.\n\nFields to extract:\n{fields_list}"
        );

        let mut properties = serde_json::Map::new();
        let mut required = vec!["reasoning".to_string()];
        let mut property_ordering = vec!["reasoning".to_string()];
        properties.insert(
            "reasoning".to_string(),
            json!({"type": "STRING", "description": "Brief reasoning about what you found"}),
        );
        for field in fields {
            let ftype = field_types.get(field).map(|s| s.as_str()).unwrap_or("string");
            let description = Self::field_description(field, ftype);
            properties.insert(field.clone(), json!({"type": "STRING", "description": description}));
            required.push(field.clone());
            property_ordering.push(field.clone());
        }

        let response_schema = json!({
            "type": "OBJECT",
            "properties": properties,
            "required": required,
            "propertyOrdering": property_ordering,
        });

        // Cached calls are for large docs — balance accuracy vs time
        let generation_config = json!({
            "responseMimeType": "application/json",
            "responseSchema": response_schema,
            "temperature": 0.0,
            "thinkingConfig": {"thinkingBudget": 4096},
        });

        let contents = json!([{"parts": [{"text": prompt}]}]);

        match self.call_gemini_cached(cache_name, contents, generation_config).await {
            Ok(parsed) => {
                match Self::extract_text_from_response(&parsed) {
                    Ok(text) => Self::parse_extraction_response(&text, fields),
                    Err(e) => {
                        error!("No text in cached extraction response: {e}");
                        fields.iter().map(|f| (f.clone(), "N/A".to_string())).collect()
                    }
                }
            }
            Err(e) => {
                error!("Cached extraction failed: {e}");
                fields.iter().map(|f| (f.clone(), "N/A".to_string())).collect()
            }
        }
    }

    /// Extract fields from a single document.
    /// With per-document extraction, each call gets manageable context (20-200K chars).
    /// All fields are extracted at once — no batching needed.
    pub async fn extract_fields(
        &self,
        _offer_id: &str,
        insurer: &str,
        segment: &str,
        fields: &[String],
        field_types: &HashMap<String, String>,
        documents_text: &str,
        rfp_text: Option<&str>,
        doc_uris: &[(String, String)],
    ) -> HashMap<String, String> {
        self.extract_fields_single_batch(
            insurer, segment, fields, field_types, documents_text, rfp_text, doc_uris,
        ).await
    }
}
