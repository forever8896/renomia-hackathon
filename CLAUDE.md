# Renomia AI Hackathon — Challenge 1: Insurance Offer Comparison

## Project Overview

Build a `/solve` REST endpoint in **Rust** that receives OCR-extracted text from multiple Czech insurance offers, extracts dynamic fields, ranks offers, and identifies the best one.

**Evaluation criteria:** Scoring accuracy (60% extraction + 25% ranking + 15% best offer), run time, memory usage, and token usage.

---

## External Resources

### Challenge Repositories
- Challenge 1 (Insurance Offer Comparison): https://github.com/jiriem/renomia-hackathon-challenge-1
- Challenge 2 (Document Data Extraction): https://github.com/jiriem/renomia-hackathon-challenge-2
- Challenge 3 (Vehicle Pricing >3.5t): https://github.com/jiriem/renomia-hackathon-challenge-3

### Training Database (read-only)
- See `.env.training` for credentials (not committed to git)
- Tables: `challenges` (3 rows), `training_data` (63 rows — training rows for challenge_id=1)

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

## Required API Contract

### Endpoints
- `GET /` → `{"status": "ok"}` — Knative startup probe, must respond on port 8080
- `GET /metrics` → `{"gemini_request_count": N, "prompt_tokens": N, "completion_tokens": N, "total_tokens": N}`
- `POST /metrics/reset` → resets all counters, returns `{"status": "reset"}`
- `POST /solve` → main comparison endpoint (see schemas below)

### Environment Variables
- `GEMINI_API_KEY` — Gemini API key (provided at deploy time)
- `DATABASE_URL` — PostgreSQL connection string (default: `postgresql://hackathon:hackathon@localhost:5432/hackathon`)

### Deployment Config (must preserve)
- `service.yaml` — Knative multi-container: app (port 8080, 512Mi/1CPU) + PostgreSQL sidecar (256Mi/0.5CPU)
- `cloudbuild.yaml` — 3-step pipeline: docker build → push to Artifact Registry → deploy via `gcloud run services replace`
- `docker-compose.yml` — local dev: app + PostgreSQL 15 Alpine
- `init.sql` — creates `cache` table (key TEXT, value JSONB) — ephemeral, lost on scale-to-zero

### Resource Limits
- App container: 512Mi memory, 1 CPU
- PostgreSQL sidecar: 256Mi memory, 0.5 CPU
- Scaling: min 1, max 3 replicas
- Concurrency: 80 requests per instance
- Timeout: 300s per request

---

## Input / Output Schemas

### Input
```json
{
  "segment": "auta",
  "fields_to_extract": ["Roční pojistné", "Povinné ručení – limit", ...],
  "field_types": {
    "Roční pojistné": "number",
    "Povinné ručení – limit": "number",
    "Havarijní pojištění – limit": "string"
  },
  "offers": [
    {
      "id": "allianz",
      "insurer": "Allianz",
      "label": "Allianz",
      "documents": [
        {
          "filename": "Allianz_Redigováno.pdf",
          "ocr_text": "... OCR extracted text ...",
          "pdf_url": "https://storage.googleapis.com/..."
        }
      ]
    }
  ],
  "rfp": {
    "filename": "poptavka.pdf",
    "ocr_text": "... RFP text ...",
    "pdf_url": "https://storage.googleapis.com/..."
  }
}
```

### Key Input Fields
| Field | Description |
|-------|-------------|
| `segment` | Insurance segment (odpovědnost, auta, lodě, majetek, ...) |
| `fields_to_extract` | Ordered list of Czech field names to extract |
| `field_types` | Type of each field: `"number"` or `"string"` |
| `offers` | Array of offers, each with id, insurer, label, documents |
| `offers[].documents` | Array of docs with `filename`, `ocr_text`, `pdf_url` |
| `rfp` | Optional Request for Proposal document |

### Output
```json
{
  "offers_parsed": [
    {
      "id": "allianz",
      "insurer": "Allianz",
      "fields": {
        "Roční pojistné": "125000",
        "Povinné ručení – limit": "100000000"
      }
    }
  ],
  "ranking": ["allianz", "generali"],
  "best_offer_id": "allianz"
}
```

### Field Value Format
- **Number fields**: Return numeric value as string. Formats like `"50000000"`, `"50 000 000"`, `"CZK 150,000,000"` accepted — scorer parses numerically with ±10% tolerance
- **String fields**: Return text value. Scored with fuzzy matching after normalization (case-insensitive, whitespace-collapsed). Similarity >50% gets partial credit
- **Missing values**: Return `"N/A"` for fields not found in documents

---

## Scoring Rules

| Component | Weight | Details |
|-----------|--------|---------|
| Field extraction | 60% | Per-field scores averaged across all offers and fields |
| Ranking order | 25% | Correct relative ordering (partial credit for close positions) |
| Best offer ID | 15% | Exact match on top pick |

### Scoring Details
- **Number fields**: Exact = 1.0, ±10% = partial (0.5–1.0), ±20% = 0.25, beyond = 0.0
- **String fields**: Exact (normalized) = 1.0, fuzzy >50% = partial, below = 0.0
- **Ranking**: Correct position = 1.0, displaced by N = `max(0, 1.0 - N*0.25)`
- **Best offer**: 1.0 if correct, 0.0 otherwise

---

## Training Data Segments

| Segment | Fields | Insurers | Docs | Description |
|---------|--------|----------|------|-------------|
| odpovědnost | 66 | 4 | 11 | Liability insurance — has RFP, multiple docs per insurer |
| auta | 17 | 4 | 4 | Vehicle fleet insurance — one doc per insurer |
| lodě | 16 | 3 | 5 | Boat/yacht insurance — multiple docs per insurer |

---

## Ranking Logic

For each field in `fields_to_extract`, compare values across all insurers:
- **Coverage limits** (limit, pojistná částka): higher = better
- **Deductibles / spoluúčast**: lower = better
- **Premium / pojistné**: lower = better
- **String fields**: compare qualitatively (broader scope = better)

Count how many fields each insurer "wins" → rank by win count (highest = best).
`best_offer_id` = first in ranking.

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
unicode-normalization = "0.1"
tracing = "0.1"
tracing-subscriber = { version = "0.3", features = ["env-filter", "json"] }

[profile.release]
opt-level = 3
lto = true
codegen-units = 1
panic = "abort"
strip = true
```

---

## OCR Text Characteristics
- Heavily whitespace-padded (PDF OCR artifact)
- Czech with diacritics (č, š, ž, ř, ě, ů, ú, ý, á, í, ó, ď, ť, ň)
- Redacted docs ("Redigováno") — personal data replaced
- Mix of structured tables and flowing text
- Field names are in Czech, vary per segment
