# Renomia AI Hackathon — Challenge 2: Document Data Extraction

## Project Overview

Build a `/solve` REST endpoint in **Rust** that receives OCR-extracted text from Czech insurance contracts + amendments (dodatky) and returns structured JSON with 17 CRM fields. Amendments override base contract values — the solution must resolve the full document chain.

**Evaluation criteria:** Scoring accuracy, run time, memory usage, and token usage.

---

## External Resources

### Challenge Repositories
- Challenge 1 (Insurance Offer Comparison): https://github.com/jiriem/renomia-hackathon-challenge-1
- Challenge 2 (Document Data Extraction): https://github.com/jiriem/renomia-hackathon-challenge-2
- Challenge 3 (Vehicle Pricing >3.5t): https://github.com/jiriem/renomia-hackathon-challenge-3

### Training Database (read-only)
- Tables: `challenges` (3 rows), `training_data` (63 rows — 2 rows for challenge_id=2)

### GCS Bucket (training documents, read-only)
- Console: https://console.cloud.google.com/storage/browser/renomia-ai-hackathon-hackathon-training-docs
- JSON API list: https://storage.googleapis.com/storage/v1/b/renomia-ai-hackathon-hackathon-training-docs/o
- Individual file: `https://storage.googleapis.com/renomia-ai-hackathon-hackathon-training-docs/{path}`

### Gemini API
- Key: TBD (provided during hackathon)
- Endpoint: `generativelanguage.googleapis.com`
- Models: `gemini-2.0-flash` (fast/cheap), `gemini-2.5-pro` (accurate/expensive)

### Deployment Target
- Google Cloud Run (Knative), port 8080
- Scaling: 1–3 replicas, 80 concurrent requests, 300s timeout
- PostgreSQL sidecar container for caching
- CI/CD: `cloudbuild.yaml` → Docker build → push to Artifact Registry → deploy via `gcloud run services replace`

---

## Input / Output Schemas

### Input
```json
{
  "documents": [
    { "filename": "string (PDF filename)", "ocr_text": "string (OCR text, Czech)" }
  ]
}
```
- 2–5 documents per request, unordered
- Total OCR text: 12–70KB per request

### Output (17 fields)
```json
{
  "contractNumber": "string",
  "insurerName": "string",
  "state": "draft | accepted | cancelled",
  "assetType": "other | vehicle",
  "concludedAs": "agent | broker",
  "contractRegime": "individual | frame | fleet | coinsurance",
  "startAt": "DD.MM.YYYY | null",
  "endAt": "DD.MM.YYYY | null",
  "concludedAt": "DD.MM.YYYY | null",
  "installmentNumberPerInsurancePeriod": 1 | 2 | 4 | 12,
  "insurancePeriodMonths": 12 | 6 | 3 | 1,
  "premium": { "currency": "string (ISO 4217 lowercase)", "isCollection": true | false },
  "actionOnInsurancePeriodTermination": "auto-renewal | policy-termination",
  "noticePeriod": "string | null",
  "regPlate": "string | null",
  "latestEndorsementNumber": "string | null",
  "note": "string (2-3 sentence summary)"
}
```

---

## Scoring Rules

| Category | Fields | Rule |
|----------|--------|------|
| Enums (exact) | state, assetType, concludedAs, contractRegime, actionOnInsurancePeriodTermination | Exact match |
| Booleans (exact) | premium.isCollection | Exact match |
| Dates (exact string) | startAt, endAt, concludedAt | Exact DD.MM.YYYY string match |
| Numbers (±10%) | installmentNumberPerInsurancePeriod, insurancePeriodMonths | ±10% (but discrete, so effectively exact) |
| Strings (fuzzy) | contractNumber, insurerName, noticePeriod, regPlate, latestEndorsementNumber, note, premium.currency | Fuzzy match |

**Critical:** Dates must be zero-padded (`14.08.2023` not `14.8.2023`). Enums are lowercase English. Currency is lowercase ISO 4217. Null = JSON null, not empty string.

---

## Training Examples

### Example 1 (id=2, 5 documents)
| Filename | Type | OCR chars |
|----------|------|-----------|
| PS 3301 0150 23_Redigováno.pdf | Base contract | 28,995 |
| VPP_odpovědnost za újmu_CAS 01-052017_Redigováno.pdf | General terms | 32,776 |
| D1 k PS č. 3301015023 - podepsaný_Redigováno.pdf | Amendment 1 | 3,036 |
| D2 k PS 3301 0150 23_Redigováno.pdf | Amendment 2 | 2,597 |
| D3 k PS 3301 0150 23_Redigováno.pdf | Amendment 3 | 2,413 |

**Expected:** contractNumber="3301 0150 23", insurerName="Colonnade Insurance S.A.", state="accepted", assetType="other", concludedAs="broker", contractRegime="individual", startAt="14.08.2023", endAt="13.08.2026", concludedAt="14.08.2023", installmentNumberPerInsurancePeriod=1, insurancePeriodMonths=12, premium={currency:"czk",isCollection:true}, actionOnInsurancePeriodTermination="policy-termination", noticePeriod=null, regPlate=null, latestEndorsementNumber="3", note="Pojištění odpovědnosti za újmu..."

### Example 2 (id=3, 2 documents)
| Filename | Type | OCR chars |
|----------|------|-----------|
| PS_Redigováno.pdf | Base contract | 12,105 |
| Pojistná smlouva_Redigováno.pdf | Contract doc | 12,163 |

**Expected:** contractNumber="C555010631", insurerName="Allianz", state="accepted", assetType="other", concludedAs="broker", contractRegime="individual", startAt="17.01.2022", endAt=null, concludedAt="17.01.2022", installmentNumberPerInsurancePeriod=1, insurancePeriodMonths=12, premium={currency:"czk",isCollection:false}, actionOnInsurancePeriodTermination="auto-renewal", noticePeriod="six-weeks", regPlate=null, latestEndorsementNumber="DOP 098", note="Exclusions for infectious diseases..."

### Key Observations
- `isCollection: true` = premium paid via broker account ("inkasní makléř" / "inkaso")
- `isCollection: false` = premium paid directly to insurer ("přímá platba" / "platba na účet pojistitele" / no mention of broker collection)
- `"doba neurčitá"` = endAt: null (indefinite term)
- latestEndorsementNumber can be plain ("3") or prefixed ("DOP 098")
- noticePeriod is English, hyphenated lowercase ("six-weeks") or null.
  Prompt must specify format: "Return as lowercase English hyphenated words,
  e.g. 'six-weeks', 'three-months', 'one-month'. Not '6 weeks' or '6 týdnů'."
- note can be Czech or English
- VPP docs provide context but don't override fields

---

## Document Classification Patterns

| Filename Pattern | Document Type | Example |
|------------------|--------------|---------|
| `PS` or `Pojistná smlouva` (not inside Dodatek) | Base contract | PS 3301 0150 23_Redigováno.pdf |
| `D{N}` or `Dodatek č. {N}` | Amendment N | D1 k PS č. 3301015023.pdf |
| `VPP` | General insurance terms | VPP_odpovědnost za újmu.pdf |
| `DOP {N}` | Endorsement/supplement | (implied by training example 2) |

## Czech → English Term Mappings

| Czech | Field | Value |
|-------|-------|-------|
| doba neurčitá | endAt | null |
| inkasní makléř / inkaso | premium.isCollection | true |
| přímá platba / platba na účet pojistitele | premium.isCollection | false |
| roční / ročně | installmentNumberPerInsurancePeriod | 1 |
| pololetně | installmentNumberPerInsurancePeriod | 2 |
| čtvrtletně | installmentNumberPerInsurancePeriod | 4 |
| měsíčně | installmentNumberPerInsurancePeriod | 12 |
| automatická prolongace / obnova | actionOnInsurancePeriodTermination | auto-renewal |
| ukončení / zánik | actionOnInsurancePeriodTermination | policy-termination |
| makléř / zprostředkovatel | concludedAs | broker |
| agent / obchodní zástupce | concludedAs | agent |
| SPZ / RZ / registrační značka | regPlate | (value) |
| výpovědní lhůta / výpovědní doba | noticePeriod | (value) |

---

## Architecture: Compiler-Style Tiered Extraction Pipeline

### Framing
| Compiler Phase | Our Phase | What It Does |
|----------------|-----------|-------------|
| Lexer | Text Normalizer | OCR cleanup, Unicode NFC, whitespace normalization |
| Parser | Structural Analyzer | Section/table/key-value detection, doc classification + ordering |
| Semantic Analysis | 2-Tier Extraction | Gemini Flash → Gemini Pro (validation-triggered) |
| Optimizer | Validate + Escalate | Post-processing validation, Tier 2 escalation on failure |
| Code Gen | Output Builder | Final JSON + provenance + metrics |

### Extraction Pipeline
```
Pre-processing (0 tokens, <1ms):
  - Document classification from filenames (base contract / amendment / VPP)
  - Chronological ordering of amendments by number
  - latestEndorsementNumber from filenames when available (D1,D2,D3 → "3")
  - OCR text normalization (whitespace collapse, Unicode NFC)
  - VPP documents filtered out (no CRM field values, saves ~30KB tokens)

Tier 1: Gemini Flash — SINGLE CALL (fast, cheap)
  - Send ALL documents (base + amendments) in ONE call, ordered chronologically
  - Each document labeled: "[BASE CONTRACT: PS 3301 0150 23.pdf]", "[AMENDMENT 1: D1...]"
  - Prompt instructs: "Documents are in chronological order. Later amendments
    override earlier values. Return the FINAL/CURRENT values after all amendments."
  - ALL 17 CRM fields extracted via responseSchema (structured output)
  - "reasoning" field in schema for chain-of-thought
  - Czech glossary + enum constraints in schema descriptions
  - Total input: ~35KB in one call (vs ~37KB across 4 separate calls)
  - Saves 3x per-call overhead and latency vs per-document extraction
  → Resolves all 17 fields in most cases

Tier 2: Gemini Pro (validation-triggered, ~20% of requests)
  Triggered by concrete validation failures, NOT self-reported confidence:
  - Extracted enum value not in allowed set
  - Date parsing failure or impossible date (e.g. "32.13.2024")
  - Required field returned as null/empty
  - latestEndorsementNumber is null but amendments exist in input
  Re-extracts ONLY the failed fields with a more detailed prompt + examples
  → Fixes 0-3 fields
```

### Amendment Resolution: Single-Call with LLM Merge
- All documents sent in ONE Gemini call, ordered chronologically with clear labels
- The LLM sees the full context and resolves the amendment chain itself
- Prompt explicitly instructs: "Later amendments override earlier provisions.
  Return the FINAL values reflecting all amendments."
- This is more token-efficient (one call vs N calls) and gives the LLM full
  context for cross-document reasoning
- If no amendments exist, it's just a single-document extraction
- Debuggability: the "reasoning" field in the schema MUST include per-document
  notes (e.g. "Amendment 2 changed endAt from X to Y") so extraction errors
  can be traced back to specific documents post-hoc

### Post-Processing Pipeline
1. Enum validation — if value not in allowed set → trigger Tier 2 re-extraction
2. Date normalization — `D.M.YYYY` → `DD.MM.YYYY`, `"doba neurčitá"` → null
3. Currency lowercase — `CZK` → `czk`
4. Cross-field consistency (soft checks, log warnings but do NOT override LLM values):
   - startAt < endAt (when endAt is not null)
   - These are sanity checks only — edge cases like retroactive coverage exist
5. latestEndorsementNumber — prefer filename-derived value when available,
   fall back to LLM-extracted value when filenames don't contain endorsement numbers

### Gemini API Error Handling
- Retry on 429 (rate limit) and 503 (overloaded) with exponential backoff: 1s, 2s, 4s, max 3 retries
- Timeout per call: 30s (Flash), 60s (Pro)
- If Gemini is down after retries: return HTTP 503 with error message (returning 16 nulls would score worse than failing loudly)
- If structured output deserialization fails: retry once with same model before escalating to Tier 2
- Implement retry logic manually in gemini.rs (simple loop + tokio::time::sleep)

### Token Tracking (REQUIRED)
Every Gemini API call MUST track and return:
- `prompt_tokens`: tokens sent to model
- `completion_tokens`: tokens received from model
- `total_tokens`: sum of above
- Use Gemini's `usageMetadata` from the API response to get exact counts
- Aggregate per-request: total across all tiers, broken down by tier
- Return in response metrics so we can optimize token budget

---

## Rust Stack

### Dependencies
```toml
[dependencies]
axum = "0.8"
tokio = { version = "1", features = ["full"] }
reqwest = { version = "0.12", default-features = false, features = ["json", "rustls-tls"] }
serde = { version = "1", features = ["derive"] }
serde_json = "1"
# Gemini responseSchema is a restrictive JSON Schema subset (no $ref, oneOf, anyOf).
# Build the schema as a serde_json::Value by hand in gemini.rs — don't use schemars.
chrono = "0.4"
unicode-normalization = "0.1"
tracing = "0.1"
tracing-subscriber = { version = "0.3", features = ["env-filter", "json"] }

[profile.release]
opt-level = 3              # Optimize for speed — runtime is a scoring criterion; binary size barely affects memory score
lto = true
codegen-units = 1
panic = "abort"
strip = true
```

### Project Structure
```
src/
├── main.rs                # Axum server, routes, startup, shared state
├── models.rs              # Strict serde types: Input, Output, enums, Premium
├── pipeline/
│   ├── mod.rs             # Pipeline orchestrator
│   ├── normalizer.rs      # OCR text cleanup, Unicode NFC, whitespace
│   ├── classifier.rs      # Filename → doc type + ordering + latestEndorsementNumber from filenames
│   │                      # Uses str::contains/starts_with, no regex needed
│   └── gemini.rs          # Tier 1 & 2: API client, prompt construction, token tracking, retry logic
│                          # Prompt template embedded via include_str!("prompts/extract.txt")
├── validation.rs          # Post-processing: enum/date/cross-field checks
├── metrics.rs             # Per-request token/latency/tier tracking
└── prompts/
    └── extract.txt         # Prompt template, embedded at compile time via include_str!
```

### Prompt Strategy
- Zero-shot with rich `description` fields in responseSchema (each description = mini-prompt)
- Czech insurance glossary in system instruction
- responseSchema with `enum` constraints (enforced at Gemini decoding level)
- `"reasoning"` string field in schema captures chain-of-thought inside JSON
- noticePeriod format explicitly specified: "lowercase English hyphenated words (e.g. 'six-weeks')"
- Prompt template embedded at compile time via `include_str!("prompts/extract.txt")` — no
  runtime template engine needed, no file I/O in distroless container
- VPP docs: skip by default — they contain general terms/definitions, not CRM field values.
  VPP text wastes ~30KB tokens without improving extraction accuracy.
  Only include VPP on Tier 2 retry if initial extraction returns incomplete results.

### Binary Optimization for Cloud Run
- rustls (no OpenSSL) → fully static-friendly
- distroless/cc-debian12 base image → ~15-20MB container
- Cold start: <100ms (vs Python's 2-5s)
- Shared `reqwest::Client` singleton with pre-warmed HTTP/2 connection to Gemini

---

## Submission Requirements

### GitHub Repository
- Fork template repo, implement solution
- Clean commit history throughout hackathon
- Before deadline: upload to new empty evaluation repo

### OnePager (1 PowerPoint slide)
Must answer:
- What was used? → Rust, axum, Gemini API, 2-tier extraction
- What is the novelty? → Compiler-style pipeline in Rust, validation-triggered model escalation (Flash → Pro only on failure), single-call amendment chain resolution
- Why this approach? → Optimizes all 4 axes: accuracy (validation-driven Tier 2), runtime (Rust cold start <100ms), memory (distroless ~20MB), tokens (single call, no VPP waste)

Judged on: Novelty, readability, understandability, uniqueness

---

## OCR Text Characteristics
- Heavily whitespace-padded (PDF OCR artifact)
- Czech with diacritics (č, š, ž, ř, ě, ů, ú, ý, á, í, ó, ď, ť, ň)
- Redacted docs ("Redigováno") — personal data replaced
- Mix of structured tables and flowing text
- Amendments: 2-3KB each; base contracts: 12-30KB; VPP: 30KB+
