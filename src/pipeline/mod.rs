pub mod gemini;
pub mod normalizer;
pub mod ranker;

use std::collections::HashMap;

use tracing::{info, warn};

use crate::models::{OfferParsed, SolveRequest, SolveResponse};
use gemini::GeminiClient;
use normalizer::{is_vpp_document, normalize_ocr};

const MAX_OCR_CHARS: usize = 200_000;
/// Include VPP only if primary docs total < this threshold
const SPARSE_DOC_THRESHOLD: usize = 10_000;

/// Find the largest index <= `pos` that is a valid char boundary.
fn floor_char_boundary(s: &str, pos: usize) -> usize {
    if pos >= s.len() {
        return s.len();
    }
    let mut i = pos;
    while i > 0 && !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

pub async fn solve(request: SolveRequest, gemini: &GeminiClient) -> SolveResponse {
    let fields = &request.fields_to_extract;
    let field_types = &request.field_types;
    let segment = &request.segment;

    // Pre-process RFP if present
    let rfp_text = request.rfp.as_ref().map(|rfp| {
        let normalized = normalize_ocr(&rfp.ocr_text);
        let end = if normalized.len() > 50_000 {
            floor_char_boundary(&normalized, 50_000)
        } else {
            normalized.len()
        };
        normalized[..end].to_string()
    });

    // Parallel extraction for all offers
    let extraction_futures: Vec<_> = request
        .offers
        .iter()
        .map(|offer| {
            let offer_id = offer.id.clone();
            let insurer = offer.insurer.clone();
            let segment = segment.clone();
            let fields = fields.clone();
            let field_types = field_types.clone();
            let rfp_text = rfp_text.clone();

            // Separate primary and VPP docs
            let mut primary_docs = Vec::new();
            let mut vpp_docs = Vec::new();

            for doc in &offer.documents {
                if doc.ocr_text.is_empty() {
                    continue;
                }
                let normalized = normalize_ocr(&doc.ocr_text);
                if normalized.is_empty() {
                    continue;
                }
                if is_vpp_document(&doc.filename) {
                    vpp_docs.push((doc.filename.clone(), normalized));
                } else {
                    primary_docs.push((doc.filename.clone(), normalized));
                }
            }

            let primary_total: usize = primary_docs.iter().map(|(_, t)| t.len()).sum();

            let mut doc_text = String::new();

            // Always include primary docs
            for (filename, text) in &primary_docs {
                doc_text.push_str(&format!("\n=== Document: {} ===\n", filename));
                doc_text.push_str(text);
                doc_text.push('\n');
            }

            // Include VPP only if primary docs are too sparse
            if primary_total < SPARSE_DOC_THRESHOLD {
                for (filename, text) in &vpp_docs {
                    let budget = MAX_OCR_CHARS.saturating_sub(doc_text.len());
                    if budget < 2000 {
                        break;
                    }
                    info!(
                        "Including VPP doc {} for sparse offer {} (primary={} chars)",
                        filename, offer_id, primary_total
                    );
                    doc_text.push_str(&format!(
                        "\n=== Supplementary Document: {} ===\n",
                        filename
                    ));
                    if text.len() > budget {
                        let end = floor_char_boundary(text, budget);
                        doc_text.push_str(&text[..end]);
                    } else {
                        doc_text.push_str(text);
                    }
                    doc_text.push('\n');
                }
            } else if !vpp_docs.is_empty() {
                info!(
                    "Skipping {} VPP doc(s) for offer {} (primary={} chars sufficient)",
                    vpp_docs.len(), offer_id, primary_total
                );
            }

            // Final truncation
            if doc_text.len() > MAX_OCR_CHARS {
                warn!(
                    "Truncating OCR for offer {} from {} to {} chars",
                    offer_id, doc_text.len(), MAX_OCR_CHARS
                );
                let end = floor_char_boundary(&doc_text, MAX_OCR_CHARS);
                doc_text.truncate(end);
            }

            async move {
                let extracted = gemini
                    .extract_fields(
                        &offer_id,
                        &insurer,
                        &segment,
                        &fields,
                        &field_types,
                        &doc_text,
                        rfp_text.as_deref(),
                    )
                    .await;
                (offer_id, insurer, extracted)
            }
        })
        .collect();

    let results = futures::future::join_all(extraction_futures).await;

    // Build offers_parsed and ranking input
    let mut offers_parsed = Vec::new();
    let mut ranking_input: Vec<(String, String, HashMap<String, String>)> = Vec::new();

    for (id, insurer, fields_map) in &results {
        offers_parsed.push(OfferParsed {
            id: id.clone(),
            insurer: insurer.clone(),
            fields: fields_map.clone(),
        });
        ranking_input.push((id.clone(), insurer.clone(), fields_map.clone()));
    }

    // Rank offers
    let ranking = match gemini.rank_offers(segment, &ranking_input, field_types).await {
        Ok(r) => {
            info!("Ranking result: {:?}", r);
            r
        }
        Err(e) => {
            warn!("Gemini ranking failed: {e}, using fallback");
            ranker::fallback_rank(&ranking_input)
        }
    };

    let best_offer_id = ranking.first().cloned().unwrap_or_default();

    SolveResponse {
        offers_parsed,
        ranking,
        best_offer_id,
    }
}
