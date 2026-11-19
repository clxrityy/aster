# logs

Append-only JSONL mirrors. Postgres stays the source of truth.

```
data/logs/
  conversations/2026-11-19.jsonl   every turn
  thoughts/…                       decision traces + what was learned
  levels/…                         level changes with cause
  requests/…                       questions/requests and their outcome
  memories/…                       memories created/deleted
  config/…                         config reloads and rejections
  runtime/…                        start/stop, pause, snapshots, engine switches
  training/…                       neural training progress (off by default)
```

Each line has `at`, `type` and the event fields.

```bash
tail -f data/logs/conversations/$(date -u +%F).jsonl | jq -r '.turn.role + ": " + .turn.text'
```

```bash
jq -c '{at, decision: .trace.decision, conf: .trace.confidence}' data/logs/thoughts/*.jsonl
```

In the console: `/logs thoughts 20`. On the dashboard: the Logs page. From a shell: `aster-logs thoughts 20`. All three read through the server, so they work against a VPS too.

Config: `logs.dir`, `logs.enabled` (add `"training"` to log every training tick), `logs.retention_days` (0 = forever).

← [docs](README.md)
