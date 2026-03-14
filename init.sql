-- Local sidecar DB initialization.
-- NOTE: This DB is ephemeral in Cloud Run (lost on scale-to-zero).

CREATE TABLE IF NOT EXISTS cache (
    key TEXT PRIMARY KEY,
    value JSONB,
    created_at TIMESTAMP WITH TIME ZONE DEFAULT NOW()
);
