-- One canonical migration set, embedded by aster-storage. No automatic repair
-- or identity replacement: initialization owns all three inserts atomically.
CREATE TABLE aster_identity (
    singleton BOOLEAN PRIMARY KEY DEFAULT TRUE CHECK (singleton),
    id UUID UNIQUE NOT NULL,
    created_at TIMESTAMPTZ NOT NULL,
    snapshot JSONB NOT NULL CHECK (jsonb_typeof(snapshot) = 'object'),
    CONSTRAINT identity_snapshot_id_matches CHECK (
        COALESCE(snapshot ->> 'id' = id::text, FALSE)
    ),
    CONSTRAINT identity_snapshot_created_at_matches CHECK (
        COALESCE(
            jsonb_typeof(snapshot -> 'created_at') = 'string'
            AND (snapshot ->> 'created_at')::timestamptz = created_at,
            FALSE
        )
    )
);

CREATE TABLE aster_regulatory_state (
    identity_id UUID PRIMARY KEY REFERENCES aster_identity(id),
    snapshot JSONB NOT NULL CHECK (jsonb_typeof(snapshot) = 'object'),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE aster_audit_events (
    id BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    identity_id UUID NOT NULL REFERENCES aster_identity(id),
    kind TEXT NOT NULL,
    recorded_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    metadata JSONB NOT NULL CHECK (jsonb_typeof(metadata) = 'object')
);