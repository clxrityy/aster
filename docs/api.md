# api

All routes except `/health/live` need `Authorization: Bearer <ASTER_OWNER_TOKEN>`.

| method | path | body / query |
|---|---|---|
| POST | `/v1/chat` | `{"text", "mode": "chat"\|"train"}` |
| GET | `/v1/turns` | `?limit=` |
| GET | `/v1/thoughts/{turn_id}` | |
| POST | `/v1/feedback` | `{"turn_id", "rating": 1\|-1, "correction"?}` |
| POST | `/v1/teach` | `{"prompt", "reply"}` |
| POST | `/v1/ingest` | `{"path"}` (inside `server.ingest_dir`) |
| GET | `/v1/outbound` | `?status=pending\|answered\|…\|all` |
| POST | `/v1/outbound/{id}/answer` | `{"text"}` |
| POST | `/v1/outbound/{id}/approve` · `/deny` | |
| GET | `/v1/runtime` | status, levels, mind stats |
| POST | `/v1/runtime/pause` · `/resume` | |
| GET | `/v1/state/history` | `?limit=` |
| GET · PATCH | `/v1/config` | PATCH `{"section.key": value}` |
| GET · POST | `/v1/mind/snapshots` | POST `{"reason"?}` |
| POST | `/v1/mind/rollback` | `{"version"}` |
| GET | `/v1/mind/corpus` | plain text |
| GET | `/v1/events` | server-sent events |
| | `/v1/memories`, `/v1/identity`, `/v1/state`, `/v1/status`, `/v1/policy` | unchanged |

```bash
curl -s -H "Authorization: Bearer $TOKEN" -H 'content-type: application/json' \
  -d '{"text":"hello"}' http://127.0.0.1:3000/v1/chat | jq .aster.text
```

```bash
curl -N -H "Authorization: Bearer $TOKEN" http://127.0.0.1:3000/v1/events
```

Errors are `{"error": code, "detail"?}`. Limits, timeouts and the rate limit come from `[server]` in `aster.toml`.

← [docs](README.md)
