use unicode_normalization::UnicodeNormalization;

/// Normalize OCR text: Unicode NFC, fix OCR artifacts, collapse whitespace, trim.
pub fn normalize_ocr(text: &str) -> String {
    let nfc: String = text.nfc().collect();
    // Fix common OCR artifacts
    let fixed = nfc
        .replace('~', " ")                // tilde used as space in some OCR
        .replace("\\tilde{c}", "č")       // LaTeX OCR artifact for č
        .replace("\\tilde{C}", "Č")       // LaTeX OCR artifact for Č
        .replace("\\%", "%")              // escaped percent
        .replace("\\&", "&")              // escaped ampersand
        .replace("\u{00A0}", " ")         // non-breaking space
        .replace("\u{200B}", "")          // zero-width space
        .replace("\u{FEFF}", "");         // BOM

    let mut result = String::with_capacity(fixed.len());
    let mut prev_space = false;
    for ch in fixed.chars() {
        if ch.is_whitespace() && ch != '\n' {
            if !prev_space {
                result.push(' ');
                prev_space = true;
            }
        } else {
            prev_space = false;
            result.push(ch);
        }
    }
    result.trim().to_string()
}

/// Extract key monetary values and their context from OCR text.
/// Returns a summary section to prepend before the raw text.
pub fn extract_value_summary(text: &str) -> String {
    use std::collections::BTreeSet;

    let mut values = BTreeSet::new();

    // Find monetary values: digits followed by currency or preceded by it
    // Pattern: number (with spaces/dots as thousands sep) + Kč/CZK/EUR/mil/tis
    let chars: Vec<char> = text.chars().collect();
    let len = chars.len();
    let mut i = 0;

    while i < len {
        // Look for digit sequences that might be monetary values
        if chars[i].is_ascii_digit() {
            let start = i;
            // Consume digits, spaces, dots, commas (number formatting)
            while i < len && (chars[i].is_ascii_digit() || chars[i] == ' ' || chars[i] == '.' || chars[i] == ',' || chars[i] == '\u{00a0}') {
                i += 1;
            }
            let num_end = i;

            // Check if followed by currency or preceded by relevant context
            let after: String = chars[num_end..std::cmp::min(num_end + 20, len)].iter().collect();
            let before_start = if start > 40 { start - 40 } else { 0 };
            let before: String = chars[before_start..start].iter().collect();

            let has_currency = after.contains("Kč") || after.contains("CZK") || after.contains("EUR")
                || after.contains("mil") || after.contains("tis");
            let has_context = before.contains("limit") || before.contains("pojistn")
                || before.contains("spoluúčast") || before.contains("částk")
                || before.contains("celkem") || before.contains("sleva")
                || before.contains("premium") || before.contains("deductible");

            // Extract the number
            let num_str: String = chars[start..num_end].iter().collect();
            let digits_only: String = num_str.chars().filter(|c| c.is_ascii_digit()).collect();

            // Only include significant numbers (at least 3 digits)
            if digits_only.len() >= 3 && (has_currency || has_context) {
                // Get surrounding context (10 words each side)
                let ctx_start = if start > 60 { start - 60 } else { 0 };
                let ctx_end = std::cmp::min(num_end + 60, len);
                let context: String = chars[ctx_start..ctx_end].iter().collect();
                let context = context.replace('\n', " ").trim().to_string();
                if context.len() > 10 {
                    values.insert(context);
                }
            }
        }
        i += 1;
    }

    if values.is_empty() {
        return String::new();
    }

    let mut summary = String::from("=== KEY VALUES DETECTED IN DOCUMENT ===\n");
    for (idx, val) in values.iter().enumerate() {
        if idx >= 30 { break; } // Limit to 30 most relevant
        summary.push_str(&format!("- {}\n", val));
    }
    summary.push_str("=== END KEY VALUES ===\n\n");
    summary
}

/// For each field to extract, find relevant text snippets by keyword matching.
/// Returns a focused "field hints" section to prepend before the raw text.
pub fn extract_field_hints(text: &str, fields: &[String]) -> String {
    let text_lower = text.to_lowercase();
    let mut hints = Vec::new();

    for field in fields {
        let field_lower = field.to_lowercase();

        // Generate search terms from field name
        let search_terms: Vec<&str> = field_lower
            .split(|c: char| c == ' ' || c == '–' || c == '-' || c == ':')
            .filter(|s| s.len() > 3)
            .collect();

        if search_terms.is_empty() {
            continue;
        }

        // Find the best matching position in text
        let mut best_pos = None;
        let mut best_score = 0;

        // Try to find the full field name first
        if let Some(pos) = text_lower.find(&field_lower) {
            best_pos = Some(pos);
            best_score = 100;
        }

        // Try partial matches if full match not found
        if best_score < 100 {
            for (i, _) in text_lower.match_indices(search_terms[0]) {
                let window: String = text_lower[i..std::cmp::min(i + 200, text_lower.len())].to_string();
                let score: usize = search_terms.iter()
                    .filter(|t| window.contains(*t))
                    .count();
                if score > best_score {
                    best_score = score;
                    best_pos = Some(i);
                }
            }
        }

        // Extract context around the match (use text_lower for positions, it has same byte layout)
        if let Some(pos) = best_pos {
            if best_score >= 1 {
                let start = if pos > 100 { pos - 100 } else { 0 };
                let end = std::cmp::min(pos + 300, text_lower.len());
                // Find safe char boundaries on text_lower (same byte positions)
                let start = {
                    let mut s = start;
                    while s > 0 && !text_lower.is_char_boundary(s) { s -= 1; }
                    s
                };
                let end = {
                    let mut e = end;
                    while e < text_lower.len() && !text_lower.is_char_boundary(e) { e += 1; }
                    std::cmp::min(e, text_lower.len())
                };
                // Use text_lower for the snippet (safe boundaries guaranteed)
                let snippet = text_lower[start..end].replace('\n', " ").trim().to_string();
                if snippet.len() > 20 {
                    hints.push(format!("[{}] ...{}...", field, snippet));
                }
            }
        }
    }

    if hints.is_empty() {
        return String::new();
    }

    let mut result = String::from("=== FIELD-RELEVANT TEXT SNIPPETS ===\n");
    for hint in hints.iter().take(40) {
        result.push_str(hint);
        result.push('\n');
    }
    result.push_str("=== END SNIPPETS ===\n\n");
    result
}

/// Returns true if the filename looks like a VPP / general conditions document.
pub fn is_vpp_document(filename: &str) -> bool {
    let lower = filename.to_lowercase();
    lower.contains("vpp")
        || lower.starts_with("pp_")
        || lower.contains("conditions")
        || lower.contains("všeobecné")
        || lower.contains("pojistné podmínky")
        || lower.contains("doplňkové pojistné")
        || lower.contains("dpp")
}
