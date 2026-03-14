use std::collections::HashMap;

/// Fallback ranking: sort by "Roční pojistné" ascending (lowest premium = best).
/// Used when Gemini ranking fails.
pub fn fallback_rank(
    offers: &[(String, String, HashMap<String, String>)],
) -> Vec<String> {
    let mut scored: Vec<(String, f64)> = offers
        .iter()
        .map(|(id, _, fields)| {
            let premium = fields
                .get("Roční pojistné")
                .and_then(|v| parse_number(v))
                .unwrap_or(f64::MAX);
            (id.clone(), premium)
        })
        .collect();

    scored.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));
    scored.into_iter().map(|(id, _)| id).collect()
}

fn parse_number(s: &str) -> Option<f64> {
    let cleaned: String = s
        .chars()
        .filter(|c| c.is_ascii_digit() || *c == '.' || *c == ',')
        .collect();
    let cleaned = cleaned.replace(',', ".");
    cleaned.parse::<f64>().ok()
}
