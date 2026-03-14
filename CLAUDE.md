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
- `isCollection: true` = premium paid via broker account ("inkasní makléř")
- `"doba neurčitá"` = endAt: null (indefinite term)
- latestEndorsementNumber can be plain ("3") or prefixed ("DOP 098")
- noticePeriod is English ("six-weeks") or null
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
| Semantic Analysis | 3-Tier Extraction | Regex → Gemini Flash → Gemini Pro |
| Optimizer | Cache + Merge | Semantic caching, amendment chain resolution |
| Code Gen | Output Builder | Final JSON + provenance + metrics |

### 3-Tier Extraction
```
Tier 0: Deterministic (0 tokens, <1ms)
  - contractNumber (regex: "č." / "číslo smlouvy" patterns)
  - dates: startAt, endAt, concludedAt (DD.MM.YYYY regex)
  - regPlate (Czech plate format regex)
  - latestEndorsementNumber (filename parsing — deterministic, never use LLM)
  - Document classification + chronological ordering
  → Resolves 4-6 fields

Tier 1: Gemini Flash (fast, cheap, ~300ms)
  - All remaining fields via responseSchema (structured output)
  - Self-reported confidence per field
  - "reasoning" field in schema for chain-of-thought
  - Czech glossary + enum constraints in schema descriptions
  → Resolves 10-12 fields

Tier 2: Gemini Pro (only when needed, ~20% of requests)
  - Re-extract ONLY fields where Tier 1 confidence < 0.8
  - More detailed prompt with examples for those specific fields
  → Fixes 0-2 fields
```

### Amendment Resolution: Layer-by-Layer
1. Extract base contract fields (Tier 0 + Tier 1)
2. For each amendment in chronological order, extract ONLY changed fields
3. Programmatic merge in Rust: later amendments override earlier values
4. More reliable and debuggable than single mega-prompt

### Post-Processing Pipeline
1. Enum validation — reject invalid values
2. Date normalization — `D.M.YYYY` → `DD.MM.YYYY`, `"doba neurčitá"` → null
3. Currency lowercase — `CZK` → `czk`
4. Cross-field consistency — startAt < endAt, concludedAt ≤ startAt
5. latestEndorsementNumber — always from filename parsing, not LLM

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
sqlx = { version = "0.8", features = ["runtime-tokio-rustls", "postgres"] }
serde = { version = "1", features = ["derive"] }
serde_json = "1"
schemars = "0.8"
regex = "1"
chrono = "0.4"
minijinja = { version = "2", features = ["builtins"] }
moka = { version = "0.12", features = ["future"] }
sha2 = "0.10"
unicode-normalization = "0.1"
tracing = "0.1"
tracing-subscriber = { version = "0.3", features = ["env-filter", "json"] }

[profile.release]
opt-level = "z"
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
│   ├── classifier.rs      # Filename → doc type + amendment ordering
│   ├── regex_extractor.rs # Tier 0: deterministic extraction
│   ├── gemini.rs          # Tier 1 & 2: API client, prompt building, token tracking
│   └── merger.rs          # Amendment chain merge logic
├── cache.rs               # L1 (moka in-process) + L2 (PostgreSQL) cache
├── validation.rs          # Post-processing: enum/date/cross-field checks
├── metrics.rs             # Per-request token/latency/tier tracking
└── prompts/
    └── extract.j2         # Jinja2 prompt template (hot-reloadable)
```

### Prompt Strategy
- Zero-shot with rich `description` fields in responseSchema (each description = mini-prompt)
- Czech insurance glossary in system instruction
- responseSchema with `enum` constraints (enforced at Gemini decoding level)
- `"reasoning"` string field in schema captures chain-of-thought inside JSON
- VPP docs: skip unless base contract explicitly references them (saves ~30KB tokens)

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
- What was used? → Rust, axum, Gemini API, 3-tier extraction
- What is the novelty? → Compiler-style pipeline, zero-token regex tier, tiered model cascade
- Why this approach? → Optimizes all 4 axes: accuracy (3 tiers), runtime (Rust), memory (distroless), tokens (regex first)

Judged on: Novelty, readability, understandability, uniqueness

---

## OCR Text Characteristics
- Heavily whitespace-padded (PDF OCR artifact)
- Czech with diacritics (č, š, ž, ř, ě, ů, ú, ý, á, í, ó, ď, ť, ň)
- Redacted docs ("Redigováno") — personal data replaced
- Mix of structured tables and flowing text
- Amendments: 2-3KB each; base contracts: 12-30KB; VPP: 30KB+
