# configuration

Compose: `data/aster.toml`. Native: `aster.toml` in the working directory, `--config <path>`, or `ASTER_CONFIG`. Every key is optional. [`aster.example.toml`](../aster.example.toml) lists them all with defaults.

Change it live, three ways:

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
| `cognition` | n-gram order, reply length, recall, learning rate, **bounds** for self-regulation, `starter_pack` | yes |
| `regulation` | per-level baseline/decay/**bounds**, per-event deltas | yes |
| `logs` | directory, categories, retention | yes |
| `console` | server URL, trace panel, rating prompt | console restart |

Check without starting:

```bash
aster check-config
```

Secrets (`DATABASE_URL`, `ASTER_OWNER_TOKEN`) live only in `.env`.

← [docs](README.md)
