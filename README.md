# Renomia AI Hackathon — Challenge 1: Insurance Offer Comparison

**Team Kilian** | Built in Rust | Gemini 3.1 Flash Lite | Average Score: 0.76

## What It Does

A `/solve` REST endpoint that receives OCR-extracted text from multiple Czech insurance offers, extracts dynamic fields, ranks offers, and identifies the best one.

Given an input with `fields_to_extract` (17-66 Czech field names), `field_types` (number/string), and `offers` (each with OCR documents), the system:

1. **Extracts** all requested field values from each offer's documents
2. **Ranks** offers from best to worst using field-by-field comparison
3. **Identifies** the single best offer

## Architecture

```
Request → OCR Normalization → Adaptive Extraction → PDF Fallback → Deterministic Ranking → Response
```

### Phase 1: Pre-processing
- **OCR artifact normalization**: Unicode NFC, LaTeX fragments (`\tilde{c}` → `č`), tildes-as-spaces, escaped characters
- **Regex value pre-scan**: Scan document for monetary values (Kč, CZK, EUR patterns), prepend a "KEY VALUES DETECTED" summary to help the LLM locate numbers in long text
- **Cross-offer document attribution**: Detect when documents are filed under the wrong offer by scanning for insurer name frequency (≥15 mentions → reassign). Handles real-world broker/underwriter mismatches (e.g., Pantaenius quotation filed under Allianz)
- **VPP filtering**: General insurance terms (VPP) documents are deprioritized — included only if budget allows after primary documents

### Phase 2: Adaptive Extraction (Gemini 3.1 Flash Lite)
The system chooses between two strategies based on total document size per offer:

**Concatenated (<200K chars)**: All documents for an offer are sent in a single LLM call. Preserves cross-document context (e.g., Pojistná smlouva references Ujednání).

**MapReduce (≥200K chars)**: Each document is extracted independently, results merged with "first non-N/A wins" priority (primary docs before VPP). Avoids the "Lost in the Middle" problem where LLMs miss information buried in very long contexts (research: accuracy drops ~30% for middle-positioned data in 800K contexts).

**Dual-Call Ensemble**: Every extraction runs twice concurrently — once at temperature 0.0 (precise) and once at 0.15 (diverse). Results are merged: non-N/A values preferred, longer answers win conflicts. Research shows this fills 5-10% of N/A gaps caused by Gemini's MoE non-determinism.

### Phase 3: PDF Multimodal Fallback
For fields still missing (N/A or Neuvedeno) after OCR extraction:
1. Download original PDFs from GCS in parallel
2. Upload to Gemini File API
3. Re-extract missing fields using Gemini's native PDF vision
4. Merge recovered values into results

This recovers data invisible to OCR — particularly tabular premium data that OCR renders as unstructured text.

### Phase 4: Deterministic Ranking
Field-by-field comparison implemented in pure Rust (no LLM call):
- **Number fields**: Lower premiums/deductibles = better, higher limits = better
- **String fields**: "Ano" (3) > other text (2) > "Ne" (1) > "N/A" (0)
- **Tie handling**: Skip fields where all offers are tied; for segments with ≤20 fields, premium is tiebreaker when win counts are within 2
- **Best offer**: First in ranking

This eliminates LLM non-determinism from ranking (which is 25% of the score) and saves ~10-30s per request.

## Gemini API Usage

### Model: `gemini-3.1-flash-lite-preview`
Released 12 days before the hackathon. Outperformed Gemini 2.5 Pro on our task while being 2x faster and cheaper. Selected through systematic A/B testing of 6 models.

### API Features Used
| Feature | Purpose |
|---------|---------|
| `system_instruction` | Extraction persona + rules + few-shot examples |
| `responseSchema` with `propertyOrdering` | Guaranteed JSON output with correct field order |
| Per-field `description` in schema | Field-specific format guidance (e.g., "Allrisk not Allrisk Varianta Max") |
| `thinkingConfig` (2048-8192 budget) | Scaled reasoning time based on document complexity |
| `seed: 42` | Best-effort determinism (Gemini 2.5+ MoE doesn't guarantee it) |
| `temperature` diversity (0.0 + 0.15) | Ensemble diversity for better recall |
| File API upload | PDF multimodal extraction |

### Prompt Engineering
- **System instruction**: 30+ lines of Czech insurance domain knowledge, format rules, few-shot examples
- **No schema duplication**: Field names listed in prompt, format guidance only in schema (Google recommendation)
- **Field-specific descriptions**: 15+ pattern-matched descriptions (e.g., "Spoluúčast skla" → "Glass deductible - if % and min, return as formula")
- **Liability field handling**: Context-dependent return format (limit value vs Ano/Ne vs Vyloučeno)

## Endpoints

| Endpoint | Method | Description |
|----------|--------|-------------|
| `/` | GET | Health check → `{"status": "ok"}` |
| `/solve` | POST | Main extraction + ranking endpoint |
| `/metrics` | GET | Token usage counters |
| `/metrics/reset` | POST | Reset all counters |
| `/analytics` | GET | Per-request timing and token breakdown |

## Scoring Performance (Training Data)

| Segment | Score | Field Extraction | Ranking | Best Offer | Time |
|---------|-------|-----------------|---------|------------|------|
| auta (17 fields, 4 offers) | 0.89 | 82% | 100% | 100% | 15s |
| lodě (16 fields, 3 offers) | 0.81 | 69% | 100% | 100% | 20s |
| odpovědnost (66 fields, 4 offers) | 0.58 | 35% | 88% | 100% | 25s |
| **Average** | **0.76** | | | | **63s** |

## Resource Usage

| Metric | Value | Limit |
|--------|-------|-------|
| Response time | 63s | 300s |
| Memory | ~30MB | 512Mi |
| Gemini calls | 42 | — |
| Token usage | 676K | — |
| Cold start | <1s | — |

## Project Structure

```
src/
├── main.rs              # Axum server, 5 routes, AppState, per-request analytics
├── models.rs            # Input/Output serde types
├── metrics.rs           # Atomic counters + per-request logging
└── pipeline/
    ├── mod.rs           # Orchestrator: pre-process → adaptive extract → PDF fallback → rank
    ├── normalizer.rs    # OCR cleanup, VPP detection, regex value pre-scan
    ├── gemini.rs        # Gemini API client, system instructions, ensemble extraction
    └── ranker.rs        # Deterministic field-by-field ranking
```

## Key Research-Based Decisions

| Decision | Research Basis |
|----------|---------------|
| MapReduce for long docs | "Lost in the Middle" (arxiv 2307.03172) — LLMs miss info in middle of long contexts |
| Dual-call ensemble | LLM ensemble papers (arxiv 2511.15714, 2504.18884) — 5-15% accuracy improvement |
| Temperature diversity | "Control the Temperature" (arxiv 2510.01218) — temp 0 hurts recall |
| System instructions | Google docs — stronger instruction following + implicit cache hits |
| No schema duplication | Google docs — "duplicating schema in prompt degrades quality" |
| Per-field descriptions | MDPI Electronics 2025 — 91.5% accuracy with few-shot prompt engineering |
| Deterministic ranking | Eliminates 0.15-0.20 score variance from LLM non-determinism |
| Gemini 3.1 Flash Lite | Box blog — 13-point accuracy increase for extensive metadata extraction |

## Data Quality Finding

During development, we identified a training data error in the lodě segment: the Pantaenius quotation PDF was filed under the Allianz offer, with expected values (15562, 12456 CZK) not derivable from any provided document. Reported to organizers and confirmed. Our cross-offer attribution system handles such cases automatically.

## Development Journey

| Stage | Average Score | Key Change |
|-------|--------------|------------|
| Initial (Gemini 2.0 Flash) | 0.54 | Basic implementation |
| + System instructions + Pro | 0.60 | Better instruction following |
| + Cross-offer attribution | 0.63 | Handles broker/underwriter mismatches |
| + MapReduce architecture | 0.65 | Avoids lost-in-middle for long docs |
| + Gemini 3.1 Flash Lite | 0.75 | Newest model, faster + more accurate |
| + Dual ensemble + few-shot | **0.76** | Research-backed accuracy boost |

## Build & Deploy

```bash
# Local development
docker compose up --build
curl http://localhost:8080/

# Deploy to Cloud Run
git push  # Cloud Build triggers automatically
```

## Dependencies

- **axum** 0.8 — HTTP framework
- **tokio** — Async runtime
- **reqwest** — HTTP client (Gemini API, PDF downloads)
- **serde/serde_json** — JSON serialization
- **unicode-normalization** — OCR text cleanup
- **futures** — Parallel execution (join_all, join)
- **chrono** — Timestamps for analytics
