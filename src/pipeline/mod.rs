pub mod gemini;
pub mod normalizer;
pub mod ranker;

use std::collections::HashMap;

use tracing::{info, warn};

use crate::models::{OfferParsed, SolveRequest, SolveResponse};
use gemini::GeminiClient;
use normalizer::{is_vpp_document, normalize_ocr};

const MAX_OCR_CHARS: usize = 800_000;

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

    // === Pass 1: Extract from OCR text (parallel across offers) ===
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

            // Collect non-VPP PDF URLs for potential pass 2
            let pdf_urls: Vec<String> = offer
                .documents
                .iter()
                .filter(|doc| !is_vpp_document(&doc.filename))
                .filter_map(|doc| doc.pdf_url.clone())
                .filter(|url| !url.is_empty())
                .collect();

            // Build OCR text: primary docs + VPP if room
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

            let mut doc_text = String::new();
            for (filename, text) in &primary_docs {
                doc_text.push_str(&format!("\n=== Document: {} ===\n", filename));
                doc_text.push_str(text);
                doc_text.push('\n');
            }

            // Include VPP docs if there's room
            for (filename, text) in &vpp_docs {
                let budget = MAX_OCR_CHARS.saturating_sub(doc_text.len());
                if budget < 2000 {
                    break;
                }
                info!(
                    "Including VPP doc {} for offer {} ({} chars, budget {})",
                    filename, offer_id, text.len(), budget
                );
                doc_text.push_str(&format!(
                    "\n=== Supplementary Document (General Terms): {} ===\n",
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

            if doc_text.len() > MAX_OCR_CHARS {
                warn!(
                    "Truncating OCR for offer {} from {} to {} chars",
                    offer_id,
                    doc_text.len(),
                    MAX_OCR_CHARS
                );
                let end = floor_char_boundary(&doc_text, MAX_OCR_CHARS);
                doc_text.truncate(end);
            }

            async move {
                // Pass 1: OCR-only extraction (no PDFs)
                let extracted = gemini
                    .extract_fields(
                        &offer_id,
                        &insurer,
                        &segment,
                        &fields,
                        &field_types,
                        &doc_text,
                        rfp_text.as_deref(),
                        &[], // no PDFs in pass 1
                    )
                    .await;

                (offer_id, insurer, extracted, pdf_urls, doc_text, rfp_text)
            }
        })
        .collect();

    let pass1_results = futures::future::join_all(extraction_futures).await;

    // === Pass 2: PDF fallback for offers with N/A fields ===
    let mut final_results: Vec<(String, String, HashMap<String, String>)> = Vec::new();

    // Collect offers that need PDF fallback
    let mut pdf_tasks = Vec::new();
    let mut no_pdf_results = Vec::new();

    for (offer_id, insurer, fields_map, pdf_urls, doc_text, rfp_text) in pass1_results {
        let missing_count = fields_map
            .values()
            .filter(|v| matches!(v.as_str(), "N/A" | "Neuvedeno"))
            .count();

        if missing_count > 0 && !pdf_urls.is_empty() {
            let na_fields: Vec<String> = fields_map
                .iter()
                .filter(|(_, v)| matches!(v.as_str(), "N/A" | "Neuvedeno"))
                .map(|(k, _)| k.clone())
                .collect();

            info!(
                "Pass 2 (PDF): {} N/A fields for {} — downloading {} PDFs",
                missing_count, offer_id, pdf_urls.len()
            );

            pdf_tasks.push((offer_id, insurer, fields_map, pdf_urls, na_fields, doc_text, rfp_text));
        } else {
            no_pdf_results.push((offer_id, insurer, fields_map));
        }
    }

    // Process PDF fallbacks in parallel
    let pdf_futures: Vec<_> = pdf_tasks
        .into_iter()
        .map(|(offer_id, insurer, mut fields_map, pdf_urls, na_fields, doc_text, rfp_text)| {
            async move {
                // Download all PDFs in parallel
                let download_futures: Vec<_> = pdf_urls
                    .iter()
                    .map(|url| gemini.download_pdf(url))
                    .collect();
                let downloads = futures::future::join_all(download_futures).await;

                // Collect successful downloads
                let mut pdf_bytes: Vec<Vec<u8>> = Vec::new();
                for (result, url) in downloads.into_iter().zip(pdf_urls.iter()) {
                    match result {
                        Ok(bytes) => {
                            info!(
                                "Downloaded PDF for {}: {} ({} bytes)",
                                offer_id,
                                url.split('/').last().unwrap_or("?"),
                                bytes.len()
                            );
                            pdf_bytes.push(bytes);
                        }
                        Err(e) => warn!("Failed to download PDF {}: {}", url, e),
                    }
                }

                // Upload all in parallel
                let upload_futures: Vec<_> = pdf_bytes
                    .iter()
                    .map(|bytes| gemini.upload_pdf(bytes))
                    .collect();
                let uploads = futures::future::join_all(upload_futures).await;

                let pdf_uris: Vec<String> = uploads
                    .into_iter()
                    .filter_map(|r| match r {
                        Ok(uri) => Some(uri),
                        Err(e) => {
                            warn!("Failed to upload PDF: {}", e);
                            None
                        }
                    })
                    .collect();

                if !pdf_uris.is_empty() {
                    info!(
                        "Re-extracting {} N/A fields for {} with {} PDFs",
                        na_fields.len(), offer_id, pdf_uris.len()
                    );

                    let retry = gemini
                        .extract_fields(
                            &offer_id,
                            &insurer,
                            &segment,
                            &na_fields,
                            &field_types,
                            &doc_text,
                            rfp_text.as_deref(),
                            &pdf_uris,
                        )
                        .await;

                    for (field, value) in retry {
                        if value != "N/A" {
                            info!("PDF recovered [{offer_id}] {field}: {value}");
                            fields_map.insert(field, value);
                        }
                    }
                }

                (offer_id, insurer, fields_map)
            }
        })
        .collect();

    let pdf_results = futures::future::join_all(pdf_futures).await;

    // Combine all results
    final_results.extend(no_pdf_results);
    final_results.extend(pdf_results);

    // Sort to match input order
    let offer_order: Vec<String> = request.offers.iter().map(|o| o.id.clone()).collect();
    final_results.sort_by_key(|(id, _, _)| {
        offer_order.iter().position(|o| o == id).unwrap_or(usize::MAX)
    });

    // Build response
    let mut offers_parsed = Vec::new();
    let mut ranking_input: Vec<(String, String, HashMap<String, String>)> = Vec::new();

    for (id, insurer, fields_map) in &final_results {
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
