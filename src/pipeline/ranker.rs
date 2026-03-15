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
                // Only award if there's actual differentiation (not all same value)
                let valid_vals: Vec<f64> = parsed.iter().filter_map(|(_, v)| *v).collect();
                let all_same = valid_vals.len() > 1 && valid_vals.windows(2).all(|w| (w[0] - w[1]).abs() < 0.01);
                if !all_same {
                    *win_counts.entry(winner).or_insert(0) += 1;
                }
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
            let winners_count = scored.iter().filter(|(_, s)| *s == best_score).count();
            // Only award wins when there's a clear winner (not everyone tied)
            if best_score > 0 && winners_count < scored.len() {
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

    // For few fields (<=20): treat 1-2 win differences as ties, use premium
    // For many fields (>20): use strict win count (coverage breadth matters more)
    let use_premium_tiebreak = all_fields.len() <= 20;

    ranked.sort_by(|a, b| {
        if use_premium_tiebreak {
            let win_diff = (b.1 as i64) - (a.1 as i64);
            if win_diff.abs() <= 2 {
                a.2.partial_cmp(&b.2).unwrap_or(std::cmp::Ordering::Equal)
                    .then_with(|| b.1.cmp(&a.1))
            } else {
                b.1.cmp(&a.1)
                    .then_with(|| a.2.partial_cmp(&b.2).unwrap_or(std::cmp::Ordering::Equal))
            }
        } else {
            b.1.cmp(&a.1)
                .then_with(|| a.2.partial_cmp(&b.2).unwrap_or(std::cmp::Ordering::Equal))
        }
    });

    ranked.into_iter().map(|(id, _, _)| id.to_string()).collect()
}
