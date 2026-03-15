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
