# Challenge 1: Insurance Offer Comparison — Solution Overview

## Slide 1: Architecture & Approach

### Why Rust?
- **Fastest cold start** on Cloud Run (vs Python/Node) — <1s startup
- **512Mi memory limit** easily met — Rust uses ~30MB vs Python ~200MB+
- Zero-cost abstractions, no GC pauses during request processing

### Extraction Pipeline (3-Pass Adaptive)
```
Pass 1: OCR Text Extraction
  - Hybrid strategy: concatenated for <200K, MapReduce for >200K
  - Gemini 2.5 Pro with system instructions + thinking mode
  - Per-field schema descriptions guide extraction format
  - Parallel across all offers (tokio + futures::join_all)

Pass 2: PDF Multimodal Fallback (adaptive)
  - Only triggered for fields still N/A after Pass 1
  - Downloads original PDFs from GCS in parallel
  - Uploads to Gemini File API for native PDF vision
  - Recovers values invisible to OCR (tables, layout)

Pass 3: Deterministic Ranking (no LLM)
  - Field-by-field comparison in Rust (zero latency)
  - Number fields: lower premium/deductible = better, higher limits = better
  - String fields: Ano > Ne > N/A (skip ties)
  - Premium tiebreaker for segments with few fields
```

### Novel Techniques
- **Cross-offer document attribution**: Detects broker/underwriter mismatches (e.g., Pantaenius docs filed under Allianz) by scanning for insurer name frequency
- **Smart VPP filtering**: General terms included only when budget allows, skipped for MapReduce
- **OCR artifact normalization**: LaTeX fragments, tildes-as-spaces, Unicode NFC

---

## Slide 2: Technical Details & KPIs

### Gemini API Optimizations
| Feature | Impact |
|---------|--------|
| System Instructions | Better instruction following, implicit cache hits |
| Thinking Mode (2-8K budget) | Deeper reasoning for complex documents |
| responseSchema with propertyOrdering | Guaranteed JSON, correct field order |
| Per-field descriptions in schema | Field-specific format guidance |
| No schema duplication in prompt | Google-recommended, improves quality |

### Measured KPIs (Training Data)

| Metric | auta (17 fields) | lode (16 fields) | odpovednost (66 fields) |
|--------|-----------------|-------------------|------------------------|
| Field extraction | ~85% | ~30-40% | ~27-31% |
| Ranking accuracy | 100% | 83-100% | 87-100% |
| Best offer | 100% | 100% | 100% |
| **Composite score** | **~0.89** | **~0.54** | **~0.55** |
| Response time | ~40s | ~45s | ~90s |
| Gemini calls | 8 | 5 | 8-13 |
| Token usage | ~70K | ~35K | ~210K |

### Resource Efficiency
- **Total tokens**: ~315K per full evaluation (all 3 segments)
- **Total time**: ~170s (57% of 300s limit)
- **Memory**: <50MB (10% of 512Mi limit)
- **Gemini calls**: ~21 total (optimized from 30+ with architecture changes)

---

## Slide 3: Why This Is The Best Approach

### Competitive Advantages
1. **Rust** — No other team uses it. Fastest deploy, lowest resource usage, most reliable under load
2. **Adaptive extraction** — Concatenated for short docs (preserves cross-reference context), MapReduce for long docs (avoids lost-in-middle)
3. **PDF multimodal fallback** — Recovers data invisible to OCR by using Gemini's native PDF vision
4. **Cross-offer attribution** — Handles real-world data quality issues (misattributed broker docs)
5. **Deterministic ranking** — Zero LLM variance, saves tokens and time

### Architecture Decisions
- **Why Gemini 2.5 Pro** for extraction: Best accuracy for structured data from messy OCR
- **Why deterministic ranking**: LLM ranking was non-deterministic (varied by 0.2 between runs)
- **Why per-field schema descriptions**: Each field gets specific format guidance (e.g., "Allrisk" not "Allrisk Varianta Max")
- **Why NOT context caching for extraction**: Per-document approach eliminates need for caching while improving accuracy
- **Why adaptive threshold (200K)**: Below 200K, cross-document context helps. Above 200K, lost-in-middle hurts.

### Data Quality Finding
During development, we identified and reported a training data error to organizers (lodě segment: Pantaenius quotation PDF filed under Allianz offer with non-derivable expected values). Our cross-offer attribution system handles such cases automatically.
