-- Private owner-provided records. No content/provenance is copied into audit.
-- Rust validation additionally rejects Unicode-only whitespace and NUL bytes.
CREATE TABLE aster_memories (
    id UUID PRIMARY KEY CHECK (id <> '00000000-0000-0000-0000-000000000000'::uuid),
    identity_id UUID NOT NULL REFERENCES aster_identity(id),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now() CHECK (isfinite(created_at)),
    content TEXT NOT NULL CHECK (
        octet_length(content) BETWEEN 1 AND 16384
        AND btrim(content, E' \t\n\r\013\f') <> ''
    ),
    provenance TEXT NOT NULL CHECK (
        octet_length(provenance) BETWEEN 1 AND 1024
        AND btrim(provenance, E' \t\n\r\013\f') <> ''
    )
);

CREATE INDEX aster_memories_listing
    ON aster_memories (identity_id, created_at DESC, id DESC);