pub mod gemini;
pub mod normalizer;
pub mod ranker;

use std::collections::HashMap;

use tracing::{info, warn};

use crate::models::{OfferParsed, SolveRequest, SolveResponse};
use gemini::GeminiClient;
use normalizer::{extract_value_summary, is_vpp_document, normalize_ocr};

const MAX_DOC_CHARS: usize = 200_000;
/// Hard deadline for the entire solve — leave margin for ranking
const SOLVE_DEADLINE_SECS: u64 = 250;

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
    let start_time = std::time::Instant::now();
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

    // === Cross-offer document attribution ===
    let sparse_offers: Vec<String> = request.offers.iter()
        .filter(|o| {
            let own_text: usize = o.documents.iter()
                .filter(|d| !is_vpp_document(&d.filename))
                .map(|d| d.ocr_text.len())
                .sum();
            own_text < 15_000
        })
        .map(|o| o.id.clone())
        .collect();

    let mut cross_ref_docs: HashMap<String, Vec<(String, String)>> = HashMap::new();
    let mut cross_ref_pdfs: HashMap<String, Vec<(String, String)>> = HashMap::new();
    let mut reassigned_docs: HashMap<String, Vec<String>> = HashMap::new();

    for offer in &request.offers {
        if !sparse_offers.contains(&offer.id) {
            continue;
        }
        let offer_insurer_lower = offer.insurer.to_lowercase();
        for other_offer in &request.offers {
            if other_offer.id == offer.id {
                continue;
            }
            for doc in &other_offer.documents {
                if doc.ocr_text.is_empty() || is_vpp_document(&doc.filename) {
                    continue;
                }
                let text_lower = doc.ocr_text.to_lowercase();
                let mention_count = text_lower.matches(&offer_insurer_lower).count();
                if mention_count >= 15 {
                    info!(
                        "Cross-ref: doc '{}' (filed under {}) mentions '{}' {} times → attributing to {}",
                        doc.filename, other_offer.id, offer.insurer, mention_count, offer.id
                    );
                    let normalized = normalize_ocr(&doc.ocr_text);
                    cross_ref_docs
                        .entry(offer.id.clone())
                        .or_default()
                        .push((doc.filename.clone(), normalized));
                    reassigned_docs
                        .entry(other_offer.id.clone())
                        .or_default()
                        .push(doc.filename.clone());

                    if doc.filename.to_lowercase().ends_with(".pdf") {
                        if let Some(url) = &doc.pdf_url {
                            if !url.is_empty() {
                                cross_ref_pdfs
                                    .entry(offer.id.clone())
                                    .or_default()
                                    .push((url.clone(), "application/pdf".to_string()));
                            }
                        }
                    }
                }
            }
        }
    }

    // === Per-document extraction (Map phase) ===
    // Instead of concatenating all docs into one 800K blob,
    // extract from each document independently then merge.
    // This avoids "lost in the middle" for long contexts.
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

            let moved_away: Vec<String> = reassigned_docs
                .get(&offer_id)
                .cloned()
                .unwrap_or_default();

            // Collect all documents: own (minus reassigned) + cross-referenced
            let mut primary_docs: Vec<(String, String, bool)> = Vec::new(); // (filename, text, is_vpp)
            for doc in &offer.documents {
                if doc.ocr_text.is_empty() {
                    continue;
                }
                if moved_away.contains(&doc.filename) {
                    info!("Skipping reassigned doc '{}' from offer {}", doc.filename, offer_id);
                    continue;
                }
                let normalized = normalize_ocr(&doc.ocr_text);
                if normalized.is_empty() {
                    continue;
                }
                let is_vpp = is_vpp_document(&doc.filename);
                primary_docs.push((doc.filename.clone(), normalized, is_vpp));
            }

            // Add cross-referenced docs
            if let Some(xref_docs) = cross_ref_docs.get(&offer_id) {
                for (filename, text) in xref_docs {
                    primary_docs.push((format!("[Cross-ref] {}", filename), text.clone(), false));
                }
            }

            // Collect PDF URLs for fallback
            let mut pdf_urls: Vec<(String, String)> = offer
                .documents
                .iter()
                .filter(|doc| !is_vpp_document(&doc.filename))
                .filter(|doc| !moved_away.contains(&doc.filename))
                .filter(|doc| doc.filename.to_lowercase().ends_with(".pdf"))
                .filter_map(|doc| {
                    let url = doc.pdf_url.clone()?;
                    if url.is_empty() { return None; }
                    Some((url, "application/pdf".to_string()))
                })
                .collect();
            if let Some(xref_pdfs) = cross_ref_pdfs.get(&offer_id) {
                pdf_urls.extend(xref_pdfs.clone());
            }

            async move {
                // Sort: primary docs first, VPP last
                let mut sorted_docs = primary_docs.clone();
                sorted_docs.sort_by_key(|(_, _, is_vpp)| *is_vpp);

                // Decide strategy based on total text size:
                // - Small total (<200K): concatenate all docs for cross-reference context
                // - Large total (>=200K): per-document extraction then merge (avoid lost-in-middle)
                let total_text: usize = sorted_docs.iter().map(|(_, t, _)| t.len()).sum();

                let per_doc_results = if total_text < 200_000 {
                    // CONCATENATED: all docs in one call — preserves cross-document context
                    let mut combined = String::new();
                    for (filename, text, _) in &sorted_docs {
                        combined.push_str(&format!("\n=== Document: {} ===\n", filename));
                        combined.push_str(text);
                        combined.push('\n');
                    }
                    if combined.len() > MAX_DOC_CHARS {
                        let end = floor_char_boundary(&combined, MAX_DOC_CHARS);
                        combined.truncate(end);
                    }

                    // Prepend value summary to help LLM locate key numbers
                    let summary = extract_value_summary(&combined);
                    if !summary.is_empty() {
                        combined = format!("{}{}", summary, combined);
                    }

                    let result = gemini.extract_fields(
                        &offer_id, &insurer, &segment,
                        &fields, &field_types,
                        &combined, rfp_text.as_deref(), &[],
                    ).await;
                    vec![result]
                } else {
                    // MAP-REDUCE: extract from each doc independently, then merge
                    // Avoids lost-in-middle for very long combined contexts
                    info!("Per-doc extraction for {} ({} chars total, {} docs)", offer_id, total_text, sorted_docs.len());

                    let labeled_docs: Vec<String> = sorted_docs.iter()
                        .map(|(filename, text, _)| {
                            let mut doc_text = text.clone();
                            if doc_text.len() > MAX_DOC_CHARS {
                                let end = floor_char_boundary(&doc_text, MAX_DOC_CHARS);
                                doc_text.truncate(end);
                            }
                            let summary = extract_value_summary(&doc_text);
                            format!("{}\n=== Document: {} ===\n{}", summary, filename, doc_text)
                        })
                        .collect();

                    let doc_futures: Vec<_> = labeled_docs.iter()
                        .map(|labeled| {
                            gemini.extract_fields(
                                &offer_id, &insurer, &segment,
                                &fields, &field_types,
                                labeled, rfp_text.as_deref(), &[],
                            )
                        })
                        .collect();

                    futures::future::join_all(doc_futures).await
                };

                // REDUCE: Merge results — first non-N/A value wins
                let mut merged: HashMap<String, String> = HashMap::new();
                for field in &fields {
                    merged.insert(field.clone(), "N/A".to_string());
                }

                for doc_result in &per_doc_results {
                    for (field, value) in doc_result {
                        if value != "N/A" && value != "Neuvedeno" {
                            let current = merged.get(field).map(|s| s.as_str()).unwrap_or("N/A");
                            if current == "N/A" || current == "Neuvedeno" {
                                merged.insert(field.clone(), value.clone());
                            }
                        } else if value == "Neuvedeno" {
                            let current = merged.get(field).map(|s| s.as_str()).unwrap_or("N/A");
                            if current == "N/A" {
                                merged.insert(field.clone(), value.clone());
                            }
                        }
                    }
                }

                (offer_id, insurer, merged, pdf_urls)
            }
        })
        .collect();

    let pass1_results = futures::future::join_all(extraction_futures).await;

    // === PDF fallback for remaining N/A fields ===
    let elapsed = start_time.elapsed().as_secs();
    let time_remaining = SOLVE_DEADLINE_SECS.saturating_sub(elapsed);
    let skip_pdf = time_remaining < 60;

    if skip_pdf {
        warn!("Skipping PDF fallback — only {}s remaining", time_remaining);
    }

    let mut final_results: Vec<(String, String, HashMap<String, String>)> = Vec::new();
    let mut pdf_tasks = Vec::new();
    let mut no_pdf_results = Vec::new();

    for (offer_id, insurer, fields_map, pdf_urls) in pass1_results {
        let missing_count = fields_map
            .values()
            .filter(|v| matches!(v.as_str(), "N/A" | "Neuvedeno"))
            .count();

        if missing_count > 0 && !pdf_urls.is_empty() && !skip_pdf {
            let na_fields: Vec<String> = fields_map
                .iter()
                .filter(|(_, v)| matches!(v.as_str(), "N/A" | "Neuvedeno"))
                .map(|(k, _)| k.clone())
                .collect();

            info!(
                "PDF fallback: {} missing fields for {} — downloading {} PDFs",
                missing_count, offer_id, pdf_urls.len()
            );

            pdf_tasks.push((offer_id, insurer, fields_map, pdf_urls, na_fields));
        } else {
            no_pdf_results.push((offer_id, insurer, fields_map));
        }
    }

    // Process PDF fallbacks in parallel
    let pdf_futures: Vec<_> = pdf_tasks
        .into_iter()
        .map(|(offer_id, insurer, mut fields_map, pdf_urls, na_fields)| {
            async move {
                let download_futures: Vec<_> = pdf_urls
                    .iter()
                    .map(|(url, _mime)| gemini.download_pdf(url))
                    .collect();
                let downloads = futures::future::join_all(download_futures).await;

                let mut doc_bytes: Vec<(Vec<u8>, String)> = Vec::new();
                for (result, (url, mime)) in downloads.into_iter().zip(pdf_urls.iter()) {
                    match result {
                        Ok(bytes) => {
                            info!("Downloaded PDF for {}: {} ({} bytes)",
                                offer_id, url.split('/').last().unwrap_or("?"), bytes.len());
                            doc_bytes.push((bytes, mime.clone()));
                        }
                        Err(e) => warn!("Failed to download {}: {}", url, e),
                    }
                }

                let upload_futures: Vec<_> = doc_bytes
                    .iter()
                    .map(|(bytes, mime)| gemini.upload_document(bytes, mime))
                    .collect();
                let uploads = futures::future::join_all(upload_futures).await;

                let doc_uris: Vec<(String, String)> = uploads
                    .into_iter()
                    .zip(doc_bytes.iter().map(|(_, mime)| mime.clone()))
                    .filter_map(|(r, mime)| match r {
                        Ok(uri) => Some((uri, mime)),
                        Err(e) => { warn!("Failed to upload: {}", e); None }
                    })
                    .collect();

                if !doc_uris.is_empty() {
                    info!("Re-extracting {} fields for {} with {} PDFs",
                        na_fields.len(), offer_id, doc_uris.len());

                    let retry = gemini
                        .extract_fields(
                            &offer_id, &insurer, &segment,
                            &na_fields, &field_types,
                            "", // no OCR text needed — PDFs have the content
                            None,
                            &doc_uris,
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

    // Deterministic ranking
    let ranking = ranker::deterministic_rank(&ranking_input, field_types);
    info!("Deterministic ranking result: {:?}", ranking);

    let best_offer_id = ranking.first().cloned().unwrap_or_default();

    SolveResponse {
        offers_parsed,
        ranking,
        best_offer_id,
    }
}
