use std::collections::HashMap;

/// Deterministic ranking: score each offer by counting field "wins".
///
/// For each field, determine which offer has the best value:
/// - Number fields with "pojistné"/"premium"/"CELKEM" in name: LOWER is better
/// - Number fields with "limit"/"částka" in name: HIGHER is better
/// - Number fields with "spoluúčast"/"deductible" in name: LOWER is better
/// - String fields: "Ano" > "Ne" > "N/A"/"Neuvedeno"
///
/// Count wins per offer, rank by most wins (descending).
pub fn deterministic_rank(
    offers: &[(String, String, HashMap<String, String>)],
    field_types: &HashMap<String, String>,
) -> Vec<String> {
    if offers.is_empty() {
        return Vec::new();
    }
    if offers.len() == 1 {
        return vec![offers[0].0.clone()];
    }

    let mut win_counts: HashMap<&str, usize> = HashMap::new();
    for (id, _, _) in offers {
        win_counts.insert(id.as_str(), 0);
    }

    // Collect all field names across all offers
    let mut all_fields: Vec<String> = Vec::new();
    for (_, _, fields) in offers {
        for key in fields.keys() {
            if !all_fields.contains(key) {
                all_fields.push(key.clone());
            }
        }
    }

    for field_name in &all_fields {
        let ftype = field_types.get(field_name).map(|s| s.as_str()).unwrap_or("string");
        let lower_name = field_name.to_lowercase();

        // Collect values for this field across all offers
        let values: Vec<(&str, Option<&str>)> = offers
            .iter()
            .map(|(id, _, fields)| {
                (id.as_str(), fields.get(field_name).map(|s| s.as_str()))
            })
            .collect();

        if ftype == "number" {
            // Determine direction: lower-is-better or higher-is-better
            let lower_is_better = is_lower_better(&lower_name);

            // Parse all numeric values
            let parsed: Vec<(&str, Option<f64>)> = values
                .iter()
                .map(|(id, val)| {
                    let num = val.and_then(|v| parse_number(v));
                    (*id, num)
                })
                .collect();

            // Find the best value
            let best_id = if lower_is_better {
                parsed.iter()
                    .filter(|(_, v)| v.is_some())
                    .min_by(|a, b| a.1.unwrap().partial_cmp(&b.1.unwrap()).unwrap_or(std::cmp::Ordering::Equal))
                    .map(|(id, _)| *id)
            } else {
                parsed.iter()
                    .filter(|(_, v)| v.is_some())
                    .max_by(|a, b| a.1.unwrap().partial_cmp(&b.1.unwrap()).unwrap_or(std::cmp::Ordering::Equal))
                    .map(|(id, _)| *id)
            };

            if let Some(winner) = best_id {
                *win_counts.entry(winner).or_insert(0) += 1;
            }
        } else {
            // String field: score each value
            let scored: Vec<(&str, i32)> = values
                .iter()
                .map(|(id, val)| {
                    let score = string_score(val.unwrap_or("N/A"));
                    (*id, score)
                })
                .collect();

            let best_score = scored.iter().map(|(_, s)| *s).max().unwrap_or(0);
            if best_score > 0 {
                // Award win to all offers tied at the best score
                for (id, score) in &scored {
                    if *score == best_score {
                        *win_counts.entry(id).or_insert(0) += 1;
                    }
                }
            }
        }
    }

    // Sort by win count descending, then by premium ascending as tiebreaker
    let mut ranked: Vec<(&str, usize, f64)> = offers
        .iter()
        .map(|(id, _, fields)| {
            let wins = *win_counts.get(id.as_str()).unwrap_or(&0);
            let premium = fields
                .iter()
                .find(|(k, _)| {
                    let lk = k.to_lowercase();
                    lk.contains("celkem") || (lk.contains("pojistn") && lk.contains("ročn"))
                })
                .and_then(|(_, v)| parse_number(v))
                .unwrap_or(f64::MAX);
            (id.as_str(), wins, premium)
        })
        .collect();

    ranked.sort_by(|a, b| {
        b.1.cmp(&a.1) // more wins = better
            .then_with(|| a.2.partial_cmp(&b.2).unwrap_or(std::cmp::Ordering::Equal)) // lower premium as tiebreaker
    });

    ranked.into_iter().map(|(id, _, _)| id.to_string()).collect()
}

/// Fallback ranking: sort by "Roční pojistné" ascending (lowest premium = best).
/// Used when deterministic ranking has no data.
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

/// Determine if lower values are better for this field name.
fn is_lower_better(lower_name: &str) -> bool {
    // Premium/cost fields: lower is better
    if lower_name.contains("pojistn") // pojistné, pojistného
        || lower_name.contains("premium")
        || lower_name.contains("celkem")
        || lower_name.contains("total")
        || lower_name.contains("sleva") // discount (but lower discount = less discount, hmm)
        || lower_name.contains("spoluúčast")
        || lower_name.contains("spoluúcast") // common OCR variant
        || lower_name.contains("deductible")
        || lower_name.contains("excess")
        || lower_name.contains("selbstbeteiligung")
    {
        // But "limit" or "částka" in the name means it's a coverage amount (higher is better)
        if lower_name.contains("limit") || lower_name.contains("částk") {
            return false;
        }
        return true;
    }

    // Coverage limits: higher is better (return false = higher is better)
    if lower_name.contains("limit") || lower_name.contains("částk") {
        return false;
    }

    // Default for unknown number fields: lower is better (assumes cost)
    true
}

/// Score a string field value for comparison.
fn string_score(val: &str) -> i32 {
    let lower = val.to_lowercase().trim().to_string();
    if lower.starts_with("ano") {
        3
    } else if lower.starts_with("ne ") || lower == "ne" {
        1
    } else if lower == "n/a" || lower == "neuvedeno" || lower.is_empty() {
        0
    } else if lower.starts_with("vyloučen") {
        0
    } else {
        // Some other string value — treat as present (better than N/A)
        2
    }
}

fn parse_number(s: &str) -> Option<f64> {
    let cleaned: String = s
        .chars()
        .filter(|c| c.is_ascii_digit() || *c == '.' || *c == ',')
        .collect();
    let cleaned = cleaned.replace(',', ".");
    cleaned.parse::<f64>().ok()
}
