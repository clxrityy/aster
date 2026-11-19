-- Steering: the owner acting on a turn's candidates (prefer one, rate one,
-- mark a recalled document irrelevant, ask Aster to reconsider). Each row is
-- audited in the same transaction. The original reply is never rewritten.
CREATE TABLE aster_turn_signals (
    id BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    identity_id UUID NOT NULL REFERENCES aster_identity(id),
    turn_id UUID NOT NULL REFERENCES aster_turns(id) ON DELETE CASCADE,
    kind TEXT NOT NULL CHECK (kind IN ('prefer', 'rate', 'irrelevant', 'reconsider')),
    candidate_index INTEGER CHECK (candidate_index IS NULL OR candidate_index BETWEEN 0 AND 63),
    rating SMALLINT CHECK (rating IS NULL OR rating IN (-1, 1)),
    -- What the signal pointed at (candidate or recalled text), for the record.
    detail TEXT CHECK (detail IS NULL OR octet_length(detail) BETWEEN 1 AND 16384),
    created_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    CHECK ((kind = 'rate') = (rating IS NOT NULL)),
    CHECK ((kind = 'reconsider') = (candidate_index IS NULL))
);
CREATE INDEX aster_turn_signals_turn ON aster_turn_signals (turn_id, created_at);
