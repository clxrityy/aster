# api

All routes except `/health/live` need `Authorization: Bearer <ASTER_OWNER_TOKEN>`, or a dashboard session cookie (see [dashboard](dashboard.md)). `bin/aster-curl /v1/…` adds the header for you.

| method | path | body / query |
|---|---|---|
| POST | `/v1/chat` | `{"text", "mode": "chat"\|"train", "reply_to"?}` |
| GET | `/v1/turns` | `?limit=` |
| GET | `/v1/thoughts/{turn_id}` | |
| POST | `/v1/turns/{turn_id}/prefer` | `{"candidate": i}`: make candidate *i* of that reply's trace the reply |
| POST | `/v1/turns/{turn_id}/candidates/{i}/rate` | `{"rating": 1\|-1}` |
| POST | `/v1/turns/{turn_id}/recalled/{i}/irrelevant` | |
| POST | `/v1/turns/{turn_id}/reconsider` | a new reply to the same message |
| GET | `/v1/turns/{turn_id}/signals` | what you did to that reply |
| POST | `/v1/feedback` | `{"turn_id", "rating": 1\|-1, "correction"?}` |
| POST | `/v1/teach` | `{"prompt", "reply"}` |
| POST | `/v1/ingest` | `{"path"}` (inside `server.ingest_dir`) |
| GET | `/v1/ingest/files` | files you could ingest |
| POST | `/v1/ingest/upload` | `{"name", "text"}`: save into `uploads/` and ingest (up to `server.ingest_max_bytes`) |
| POST | `/v1/read` | `{"url"}`: read a web page ([reading](reading.md)) |
| GET | `/v1/outbound` | `?status=pending\|answered\|…\|all` |
| POST | `/v1/outbound/{id}/answer` | `{"text"}` |
| POST | `/v1/outbound/{id}/approve` · `/deny` | approving a request with a `source` URL reads that page |
| GET | `/v1/runtime` | status, levels, mind stats |
| POST | `/v1/runtime/pause` · `/resume` | |
| GET | `/v1/state/history` | `?limit=` |
| GET · PATCH | `/v1/config` | PATCH `{"section.key": value}` |
| GET · POST | `/v1/mind/snapshots` | POST `{"reason"?}` |
| POST | `/v1/mind/rollback` | `{"version"}` |
| GET | `/v1/mind/corpus` | plain text |
| GET | `/v1/mind/training` | engine, engines in this build, loss history, stats |
| GET | `/v1/logs/{category}` | `?limit=`: newest log entries |
| GET | `/v1/events` | server-sent events |
| | `/v1/memories`, `/v1/identity`, `/v1/state`, `/v1/status`, `/v1/policy` | unchanged |
| GET · POST | `/v1/session` | dashboard sign-in: POST `{"token"}` sets the session cookie |
| POST | `/v1/session/logout` | |

`{turn_id}` in the steering routes is an Aster reply. Candidate and recalled indexes are 0-based, in the order the trace lists them. The server reads the text from the stored trace, never from the request.

```bash
curl -s -H "Authorization: Bearer $TOKEN" -H 'content-type: application/json' \
  -d '{"text":"hello"}' http://127.0.0.1:3000/v1/chat | jq .aster.text
```

```bash
curl -N -H "Authorization: Bearer $TOKEN" http://127.0.0.1:3000/v1/events
```

Events include `trained` (neural training progress) besides turns, thoughts, levels, outbound, memory, learned, config and runtime.

Errors are `{"error": code, "detail"?}`. Limits, timeouts and the rate limit come from `[server]` in `aster.toml`. The rate limit counts only writes.

← [docs](README.md)
