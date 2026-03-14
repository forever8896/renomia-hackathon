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
