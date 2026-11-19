-- Runtime: conversation turns, decision traces, owner feedback, outbound
-- questions/requests, regulatory history, and versioned mind snapshots.
CREATE TABLE aster_turns (
    id UUID PRIMARY KEY,
    identity_id UUID NOT NULL REFERENCES aster_identity(id),
    conversation_id UUID NOT NULL,
    role TEXT NOT NULL CHECK (role IN ('owner', 'aster')),
    mode TEXT NOT NULL CHECK (mode IN ('chat', 'train')),
    text TEXT NOT NULL CHECK (octet_length(text) BETWEEN 1 AND 16384),
    reply_to UUID REFERENCES aster_turns(id),
    -- clock_timestamp keeps owner/aster turns of one transaction ordered.
    created_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp()
);
CREATE INDEX aster_turns_recent ON aster_turns (created_at DESC, id DESC);

CREATE TABLE aster_thoughts (
    turn_id UUID PRIMARY KEY REFERENCES aster_turns(id) ON DELETE CASCADE,
    trace JSONB NOT NULL CHECK (jsonb_typeof(trace) = 'object')
);

CREATE TABLE aster_feedback (
    id BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    turn_id UUID NOT NULL REFERENCES aster_turns(id) ON DELETE CASCADE,
    rating SMALLINT NOT NULL CHECK (rating IN (-1, 1)),
    correction TEXT CHECK (correction IS NULL OR octet_length(correction) BETWEEN 1 AND 16384),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE aster_outbound (
    id UUID PRIMARY KEY,
    identity_id UUID NOT NULL REFERENCES aster_identity(id),
    kind TEXT NOT NULL CHECK (kind IN ('question', 'request')),
    draft JSONB NOT NULL CHECK (jsonb_typeof(draft) = 'object'),
    origin_turn UUID REFERENCES aster_turns(id) ON DELETE SET NULL,
    status TEXT NOT NULL DEFAULT 'pending'
        CHECK (status IN ('pending', 'answered', 'approved', 'denied', 'expired')),
    answer TEXT CHECK (answer IS NULL OR octet_length(answer) BETWEEN 1 AND 16384),
    created_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    resolved_at TIMESTAMPTZ,
    CHECK ((status = 'pending') = (resolved_at IS NULL))
);
CREATE INDEX aster_outbound_status ON aster_outbound (status, created_at DESC);

CREATE TABLE aster_state_history (
    id BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    identity_id UUID NOT NULL REFERENCES aster_identity(id),
    recorded_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    snapshot JSONB NOT NULL CHECK (jsonb_typeof(snapshot) = 'object'),
    cause TEXT NOT NULL CHECK (octet_length(cause) BETWEEN 1 AND 256)
);
CREATE INDEX aster_state_history_recent ON aster_state_history (recorded_at DESC);

CREATE TABLE aster_mind_snapshots (
    version BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    identity_id UUID NOT NULL REFERENCES aster_identity(id),
    engine TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    reason TEXT NOT NULL,
    model BYTEA NOT NULL,
    stats JSONB NOT NULL CHECK (jsonb_typeof(stats) = 'object')
);
