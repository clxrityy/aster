# configuration

Compose: `data/aster.toml`. Native: `aster.toml` in the working directory, `--config <path>`, or `ASTER_CONFIG`. Every key is optional. [`aster.example.toml`](../aster.example.toml) lists them all with defaults.

Change it live, four ways: the dashboard's Config page, or:

```
/set runtime.tick_interval_ms 2000          # console
```

```bash
$EDITOR data/aster.toml                     # picked up within ~1s
```

```bash
curl -X PATCH -H "Authorization: Bearer $TOKEN" -H 'content-type: application/json' \
  -d '{"cognition.learning_rate": 0.4}' http://127.0.0.1:3000/v1/config
```

An invalid value is rejected and the old config stays active. The console shows why.

| section | controls | live? |
|---|---|---|
| `server` | body limit, request timeout, rate limit, ingest dir | yes |
| `database` | pool size, timeouts | restart |
| `runtime` | tick, pause, initiative limits, expiry, rest, snapshots kept | yes |
| `cognition` | engine, n-gram order, reply length, recall, learning rate, **bounds** for self-regulation, `starter_pack` | yes (engine: next tick) |
| `cognition.neural` | network size, training budget, candidate weight, when it may speak ([neural](neural.md)) | yes (size: next restore) |
| `regulation` | per-level baseline/decay/**bounds**, per-event deltas | yes |
| `logs` | directory, categories, retention | yes |
| `console` | server URL, trace panel, rating prompt | console restart |
| `dashboard` | on/off, cookie `Secure`, session idle/max, sign-in failures ([dashboard](dashboard.md)) | yes |
| `web` | reading pages: on/off, timeout, size, redirects, private addresses ([reading](reading.md)) | yes |

Check without starting:

```bash
aster check-config
```

Secrets (`DATABASE_URL`, `ASTER_OWNER_TOKEN`) live only in `.env` (or `deploy/.env` on a VPS). `aster-get` / `aster-set` from [commands](commands.md) work too.

← [docs](README.md)
