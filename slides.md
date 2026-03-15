# Challenge 1: Insurance Offer Comparison — Team Kilian

## Slide 1: Architecture

**Built in Rust** — fastest cold start (<1s), lowest memory (~30MB of 512Mi), zero GC pauses

### Extraction Pipeline
```
OCR Text → Regex Value Pre-scan → Adaptive Extraction → PDF Fallback → Deterministic Ranking
```

1. **Regex Pre-processing**: Scan for monetary values (Kč/CZK/EUR), prepend summary section
2. **Adaptive Extraction** (Gemini 3.1 Flash Lite):
   - Small doc sets (<200K): Concatenated for cross-document context
   - Large doc sets (>200K): MapReduce per-document, then merge
3. **Dual-Call Ensemble**: Run 2x concurrently, merge non-N/A values (research-backed: +5-10% recall)
4. **PDF Multimodal Fallback**: For remaining N/A fields, download original PDFs, extract with Gemini vision
5. **Cross-Offer Attribution**: Detect broker/underwriter mismatches by insurer name frequency
6. **Deterministic Ranking**: Field-by-field comparison in Rust (zero LLM variance, zero latency)

### Key Research-Based Decisions
- **Per-document extraction** avoids "Lost in the Middle" (LLM accuracy drops for info buried in 800K contexts)
- **Dual-call ensemble** exploits Gemini's MoE non-determinism as a feature (fills N/A gaps)
- **Schema-only field descriptions** (Google docs: duplicating schema in prompt degrades quality)
- **Few-shot examples** in system instruction (research: +5-8% format compliance)
- **Gemini 3.1 Flash Lite** (released 12 days ago — faster AND more accurate than 2.5 Pro)

---

## Slide 2: KPIs & Results

### Scoring Performance (Training Data)

| Segment | Composite Score | Field Extraction | Ranking | Best Offer |
|---------|----------------|-----------------|---------|------------|
| **auta** (17 fields, 4 offers) | **0.886** | 81% | 100% | 100% |
| **lodě** (16 fields, 3 offers) | **0.812** | 69% | 100% | 100% |
| **odpovědnost** (66 fields, 4 offers) | **0.588** | 37% | 88-100% | 100% |
| **Average** | **0.760** | | | |

### Resource Efficiency
| Metric | Value |
|--------|-------|
| Response time | **61s** (80% under 300s limit) |
| Gemini calls | 42 per evaluation |
| Token usage | 679K per evaluation |
| Memory | <50MB (10% of 512Mi) |
| Cold start | <1s (Rust distroless image) |

### Journey (start → final)
| Metric | Start | Final | Improvement |
|--------|-------|-------|-------------|
| Average score | 0.541 | **0.760** | **+40%** |
| Token efficiency | 104K | 679K | Smarter use |
| Architecture | Single blob | MapReduce + Ensemble | Research-driven |
| Model | Gemini 2.0 Flash | **Gemini 3.1 Flash Lite** | Latest model |

---

## Slide 3: Why This Approach Wins

### Novelty & Uniqueness
1. **Only Rust solution** — optimal for Cloud Run (fastest startup, lowest resource usage)
2. **Research-driven architecture** — "Lost in the Middle" paper informed our MapReduce switch, ensemble papers backed our dual-call approach
3. **Cross-offer document attribution** — handles real-world data quality (misattributed broker documents)
4. **PDF multimodal fallback** — recovers OCR-invisible values from original PDF tables
5. **Gemini 3.1 Flash Lite** — one of the first production uses of the newest model

### Technical Highlights
- **Adaptive extraction**: automatically chooses concatenated vs MapReduce based on document size
- **Smart field descriptions**: per-field schema guidance (e.g., "Spoluúčast skla" → "Glass deductible - formula if %")
- **OCR artifact normalization**: LaTeX fragments, tildes-as-spaces, Unicode NFC
- **Regex value pre-scan**: monetary value summary prepended to help LLM locate numbers in long text

### Data Quality Finding
Identified and reported a training data error (lodě: Pantaenius quotation filed under Allianz with non-derivable expected values). Our cross-offer attribution system handles such cases automatically.

### What We'd Do With More Time
- Post-extraction regex validation (verify extracted numbers exist in source text)
- Iterative refinement (show model its first attempt, ask to fill gaps)
- PostgreSQL caching (avoid redundant extraction on repeated evaluations)
