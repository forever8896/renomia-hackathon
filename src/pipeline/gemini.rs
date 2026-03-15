
    /// Extract fields from a single document.
    /// With per-document extraction, each call gets manageable context (20-200K chars).
    /// All fields are extracted at once — no batching needed.
    pub async fn extract_fields(
        &self,
        _offer_id: &str,
        insurer: &str,
        segment: &str,
        fields: &[String],
        field_types: &HashMap<String, String>,
        documents_text: &str,
        rfp_text: Option<&str>,
        doc_uris: &[(String, String)],
    ) -> HashMap<String, String> {
        self.extract_fields_single_batch(
            insurer, segment, fields, field_types, documents_text, rfp_text, doc_uris,
        ).await
    }
}
