pub mod gemini;
pub mod normalizer;
pub mod ranker;

use std::collections::HashMap;

use tracing::{info, warn};

use crate::models::{OfferParsed, SolveRequest, SolveResponse};
use gemini::GeminiClient;
use normalizer::{is_vpp_document, normalize_ocr};

const MAX_OCR_CHARS: usize = 800_000;
/// Hard deadline for the entire solve — leave margin for ranking call
const SOLVE_DEADLINE_SECS: u64 = 250;

fn mime_from_filename(filename: &str) -> String {
    let lower = filename.to_lowercase();
    if lower.ends_with(".pdf") {
        "application/pdf".to_string()
    } else if lower.ends_with(".docx") {
        "application/vnd.openxmlformats-officedocument.wordprocessingml.document".to_string()
    } else if lower.ends_with(".doc") {
        "application/msword".to_string()
    } else if lower.ends_with(".xlsx") {
        "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet".to_string()
    } else {
        "application/octet-stream".to_string()
    }
}

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
    // Build an index: for each insurer name, find documents from OTHER offers
    // that mention this insurer prominently (broker/underwriter mismatch detection)
    let mut cross_ref_docs: HashMap<String, Vec<(String, String)>> = HashMap::new(); // offer_id -> [(filename, normalized_text)]
    let mut cross_ref_pdfs: HashMap<String, Vec<(String, String)>> = HashMap::new(); // offer_id -> [(url, mime)]

    // Only cross-reference for offers that have very little own content
    let sparse_offers: Vec<String> = request.offers.iter()
        .filter(|o| {
            let own_text: usize = o.documents.iter()
                .filter(|d| !is_vpp_document(&d.filename))
                .map(|d| d.ocr_text.len())
                .sum();
            own_text < 15_000 // sparse = less than 15K chars of primary docs
        })
        .map(|o| o.id.clone())
        .collect();

    for offer in &request.offers {
        // Only cross-reference for sparse offers
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
                // Count mentions — needs very high count to avoid false positives
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

                    // Also grab the PDF URL if it's a .pdf
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

            // Collect PDF URLs: own docs + cross-referenced docs
            let mut doc_urls: Vec<(String, String)> = offer
                .documents
                .iter()
                .filter(|doc| !is_vpp_document(&doc.filename))
                .filter(|doc| doc.filename.to_lowercase().ends_with(".pdf"))
                .filter_map(|doc| {
                    let url = doc.pdf_url.clone()?;
                    if url.is_empty() { return None; }
                    Some((url, "application/pdf".to_string()))
                })
                .collect();
            // Add cross-referenced PDFs
            if let Some(xref_pdfs) = cross_ref_pdfs.get(&offer_id) {
                doc_urls.extend(xref_pdfs.clone());
            }

            // Build OCR text: primary docs + cross-referenced docs + VPP if room
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

            // Add cross-referenced docs as primary
            if let Some(xref_docs) = cross_ref_docs.get(&offer_id) {
                for (filename, text) in xref_docs {
                    primary_docs.push((
                        format!("[Cross-ref] {}", filename),
                        text.clone(),
                    ));
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

                (offer_id, insurer, extracted, doc_urls, doc_text, rfp_text)
            }
        })
        .collect();

    let pass1_results = futures::future::join_all(extraction_futures).await;

    // === Pass 2: PDF fallback for offers with N/A fields ===
    let mut final_results: Vec<(String, String, HashMap<String, String>)> = Vec::new();

    let elapsed = start_time.elapsed().as_secs();
    let time_remaining = SOLVE_DEADLINE_SECS.saturating_sub(elapsed);
    let skip_pdf = time_remaining < 60;

    if skip_pdf {
        warn!("Skipping PDF fallback — only {}s remaining (need 60s)", time_remaining);
    }

    // Collect offers that need PDF fallback
    let mut pdf_tasks = Vec::new();
    let mut no_pdf_results = Vec::new();

    for (offer_id, insurer, fields_map, doc_urls, doc_text, rfp_text) in pass1_results {
        let missing_count = fields_map
            .values()
            .filter(|v| matches!(v.as_str(), "N/A" | "Neuvedeno"))
            .count();

        if missing_count > 0 && !doc_urls.is_empty() && !skip_pdf {
            let na_fields: Vec<String> = fields_map
                .iter()
                .filter(|(_, v)| matches!(v.as_str(), "N/A" | "Neuvedeno"))
                .map(|(k, _)| k.clone())
                .collect();

            info!(
                "Pass 2 (PDF): {} N/A fields for {} — downloading {} PDFs",
                missing_count, offer_id, doc_urls.len()
            );

            pdf_tasks.push((offer_id, insurer, fields_map, doc_urls, na_fields, doc_text, rfp_text));
        } else {
            no_pdf_results.push((offer_id, insurer, fields_map));
        }
    }

    // Process PDF fallbacks in parallel
    let pdf_futures: Vec<_> = pdf_tasks
        .into_iter()
        .map(|(offer_id, insurer, mut fields_map, doc_urls, na_fields, doc_text, rfp_text)| {
            async move {
                // Download all documents in parallel
                let download_futures: Vec<_> = doc_urls
                    .iter()
                    .map(|(url, _mime)| gemini.download_pdf(url))
                    .collect();
                let downloads = futures::future::join_all(download_futures).await;

                // Collect successful downloads with their MIME types
                let mut doc_bytes: Vec<(Vec<u8>, String)> = Vec::new();
                for (result, (url, mime)) in downloads.into_iter().zip(doc_urls.iter()) {
                    match result {
                        Ok(bytes) => {
                            info!(
                                "Downloaded doc for {}: {} ({} bytes, {})",
                                offer_id,
                                url.split('/').last().unwrap_or("?"),
                                bytes.len(),
                                mime
                            );
                            doc_bytes.push((bytes, mime.clone()));
                        }
                        Err(e) => warn!("Failed to download {}: {}", url, e),
                    }
                }

                // Upload all in parallel
                let upload_futures: Vec<_> = doc_bytes
                    .iter()
                    .map(|(bytes, mime)| gemini.upload_document(bytes, mime))
                    .collect();
                let uploads = futures::future::join_all(upload_futures).await;

                // Collect URIs with their MIME types
                let doc_uris: Vec<(String, String)> = uploads
                    .into_iter()
                    .zip(doc_bytes.iter().map(|(_, mime)| mime.clone()))
                    .filter_map(|(r, mime)| match r {
                        Ok(uri) => Some((uri, mime)),
                        Err(e) => {
                            warn!("Failed to upload document: {}", e);
                            None
                        }
                    })
                    .collect();

                if !doc_uris.is_empty() {
                    info!(
                        "Re-extracting {} N/A fields for {} with {} PDFs",
                        na_fields.len(), offer_id, doc_uris.len()
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

    // Rank offers deterministically (no LLM call needed)
    let ranking = ranker::deterministic_rank(&ranking_input, field_types);
    info!("Deterministic ranking result: {:?}", ranking);

    let best_offer_id = ranking.first().cloned().unwrap_or_default();

    SolveResponse {
        offers_parsed,
        ranking,
        best_offer_id,
    }
}
