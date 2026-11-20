# commands

Shell helpers in [`bin/`](../bin). Put them on your PATH for this shell:

```bash
source bin/activate          # deactivate_aster to undo
```

They work out where Aster runs from what's there. Force it with `ASTER_MODE`.

| mode | when | uses |
|---|---|---|
| `deploy` | `deploy/.env` exists | `deploy/compose.yaml` ([deploy](deploy.md)) |
| `local` | `.env` has `POSTGRES_PASSWORD` | `compose.yaml` |
| `native` | otherwise | `target/release/aster` |

| command | does |
|---|---|
| `aster <cmd>` | `status`, `check-config`, `init`, `console`. Runs inside the container when the stack is up, else the native binary (built on first use) |
| `aster --remote user@host <cmd>` | the same on your VPS over SSH (repo at `~/aster`, or `ASTER_REMOTE_DIR`) |
| `aster-up` · `aster-down` | start (building as needed) and wait until live · stop, keeping all data |
| `aster-status` | mode, containers, liveness, runtime status |
| `aster-logs [service]` | follow container output (default `aster`; also `db`, `nginx`, `cloudflared`) |
| `aster-logs <category> [n]` | newest entries of a JSONL log through the API |
| `aster-get [key]` · `aster-set <key> <value>` | read or change live config. The value is TOML: `aster-set cognition.engine '"neural"'` |
| `aster-curl [curl opts] /v1/…` | call the API as the owner |
| `aster-token show` · `rotate` | print the owner token · replace it and restart Aster |
| `aster-backup` | `pg_dump` + `aster.toml` + `ingest/` into `backups/<time>/` |
| `aster-restore <dir> [--with-config]` | put a backup back (asks you to type `restore`; backs up first) |
| `aster-update` | backup, `git pull --ff-only`, rebuild, restart |

Safety:

- `.env` is read as plain `KEY=value` lines. It is never `source`d or executed.
- The token is passed to curl in a way that never shows in `ps`.
- `aster-down` refuses `-v`, because that would delete the database.

← [docs](README.md)
