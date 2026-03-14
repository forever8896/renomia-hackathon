#!/usr/bin/env python3
"""
Scoring harness for Renomia Insurance Offer Comparison.
Mimics the official scorer: 60% field extraction, 25% ranking, 15% best offer.
"""
import json
import re
import subprocess
import sys
import time

SERVER_URL = "http://localhost:8080"
TRAINING_DIR = "training"


def parse_number(s: str) -> float | None:
    if not s or s in ("N/A", "Neuvedeno"):
        return None
    cleaned = re.sub(r'[^\d.,\-]', '', s.replace(' ', ''))
    # Handle European decimals: if only commas, treat last comma as decimal
    if ',' in cleaned and '.' not in cleaned:
        parts = cleaned.rsplit(',', 1)
        if len(parts) == 2 and len(parts[1]) <= 2:
            cleaned = parts[0].replace(',', '') + '.' + parts[1]
        else:
            cleaned = cleaned.replace(',', '')
    else:
        cleaned = cleaned.replace(',', '')
    cleaned = cleaned.strip('.')
    try:
        return float(cleaned)
    except ValueError:
        return None


def score_number(expected: str, actual: str) -> float:
    en = parse_number(expected)
    an = parse_number(actual)
    if en is None or an is None:
        return score_string(expected, actual)
    if en == 0:
        return 1.0 if an == 0 else 0.0
    ratio = abs(an - en) / abs(en)
    if ratio <= 0.001:
        return 1.0
    elif ratio <= 0.1:
        return 0.5 + 0.5 * (1.0 - ratio / 0.1)
    elif ratio <= 0.2:
        return 0.25
    else:
        return 0.0


def score_string(expected: str, actual: str) -> float:
    if not expected or not actual or actual in ("N/A",):
        return 0.0
    e = re.sub(r'\s+', ' ', expected.lower().strip())
    a = re.sub(r'\s+', ' ', actual.lower().strip())
    if e == a:
        return 1.0
    # Character-level Dice similarity
    e_chars = set(enumerate(e))
    a_chars = set(enumerate(a))
    # Simpler: bigram similarity
    def bigrams(s):
        return [s[i:i+2] for i in range(len(s)-1)] if len(s) > 1 else [s]
    eb = bigrams(e)
    ab = bigrams(a)
    common = sum(1 for b in ab if b in eb)
    total = len(eb) + len(ab)
    sim = 2 * common / total if total > 0 else 0
    return sim if sim > 0.5 else 0.0


def score_ranking(expected: list, actual: list) -> float:
    scores = []
    for i, oid in enumerate(expected):
        if oid in actual:
            displacement = abs(actual.index(oid) - i)
            scores.append(max(0.0, 1.0 - displacement * 0.25))
        else:
            scores.append(0.0)
    return sum(scores) / len(scores) if scores else 0.0


def run_test(test_id: str, verbose: bool = False) -> dict:
    with open(f"{TRAINING_DIR}/input_{test_id}.json") as f:
        input_data = json.load(f)
    with open(f"{TRAINING_DIR}/expected_{test_id}.json") as f:
        expected = json.load(f)

    start = time.time()
    result = subprocess.run(
        ["curl", "-s", "-X", "POST", f"{SERVER_URL}/solve",
         "-H", "Content-Type: application/json",
         "-d", f"@{TRAINING_DIR}/input_{test_id}.json"],
        capture_output=True, text=True, timeout=300
    )
    elapsed = time.time() - start
    actual = json.loads(result.stdout)

    field_types = expected.get('field_types', input_data.get('field_types', {}))
    exp_offers = {o['id']: o['fields'] for o in expected['offers_parsed']}
    act_offers = {o['id']: o['fields'] for o in actual['offers_parsed']}

    # Field extraction scoring
    field_scores = []
    low_fields = []
    for offer_id in exp_offers:
        for field, exp_val in exp_offers[offer_id].items():
            act_val = act_offers.get(offer_id, {}).get(field, "N/A")
            ftype = field_types.get(field, "string")
            s = score_number(exp_val, act_val) if ftype == "number" else score_string(exp_val, act_val)
            field_scores.append(s)
            if s < 0.5:
                low_fields.append((offer_id, field, exp_val, act_val, s))

    avg_field = sum(field_scores) / len(field_scores) if field_scores else 0

    # Ranking
    exp_rank = expected['ranking']
    act_rank = actual['ranking']
    avg_rank = score_ranking(exp_rank, act_rank)

    # Best offer
    best_score = 1.0 if expected['best_offer_id'] == actual['best_offer_id'] else 0.0
    total = 0.6 * avg_field + 0.25 * avg_rank + 0.15 * best_score

    if verbose:
        for oid, field, ev, av, s in low_fields:
            print(f"  LOW [{oid}] {field}: exp='{ev}' got='{av}' ({s:.2f})")

    # Get metrics
    metrics = json.loads(subprocess.run(
        ["curl", "-s", f"{SERVER_URL}/metrics"],
        capture_output=True, text=True, timeout=10
    ).stdout)

    return {
        "test_id": test_id,
        "segment": input_data["segment"],
        "field_score": avg_field,
        "rank_score": avg_rank,
        "best_score": best_score,
        "total": total,
        "elapsed_s": elapsed,
        "total_fields": len(field_scores),
        "low_fields": len(low_fields),
        "ranking_expected": exp_rank,
        "ranking_actual": act_rank,
        "best_expected": expected['best_offer_id'],
        "best_actual": actual['best_offer_id'],
        "metrics": metrics,
    }


def main():
    verbose = "-v" in sys.argv or "--verbose" in sys.argv

    # Reset metrics
    subprocess.run(["curl", "-s", "-X", "POST", f"{SERVER_URL}/metrics/reset"],
                   capture_output=True, timeout=10)

    tests = [("87", "auta"), ("88", "lodě"), ("86", "odpovědnost")]
    results = []

    for test_id, name in tests:
        print(f"\n{'='*60}")
        print(f"Testing: {name} (input_{test_id})")
        print(f"{'='*60}")

        r = run_test(test_id, verbose=verbose)
        results.append(r)

        print(f"  Fields:  {r['field_score']:.3f} ({r['total_fields'] - r['low_fields']}/{r['total_fields']} good)")
        print(f"  Ranking: {r['rank_score']:.3f} {r['ranking_actual']}")
        print(f"  Best:    {r['best_score']:.1f} (exp={r['best_expected']} got={r['best_actual']})")
        print(f"  TOTAL:   {r['total']:.3f}")
        print(f"  Time:    {r['elapsed_s']:.1f}s")

    # Summary
    print(f"\n{'='*60}")
    print("SUMMARY")
    print(f"{'='*60}")

    avg_total = sum(r['total'] for r in results) / len(results)
    total_time = sum(r['elapsed_s'] for r in results)

    for r in results:
        print(f"  {r['segment']:15s} → {r['total']:.3f}")
    print(f"  {'AVERAGE':15s} → {avg_total:.3f}")
    print(f"  Total time: {total_time:.1f}s")

    # Final metrics
    metrics = json.loads(subprocess.run(
        ["curl", "-s", f"{SERVER_URL}/metrics"],
        capture_output=True, text=True, timeout=10
    ).stdout)
    print(f"\n  Gemini calls:     {metrics['gemini_request_count']}")
    print(f"  Prompt tokens:    {metrics['prompt_tokens']}")
    print(f"  Completion tokens:{metrics['completion_tokens']}")
    print(f"  Total tokens:     {metrics['total_tokens']}")


if __name__ == "__main__":
    main()
