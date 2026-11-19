# quickstart

## compose (podman), the main way

`.env`, from `.env.example`:

```
POSTGRES_PASSWORD=<openssl rand -hex 16>
ASTER_OWNER_TOKEN=<openssl rand -hex 32>
ASTER_CONFIG=data/aster.toml
```

```bash
mkdir -p data/ingest
cp aster.example.toml data/aster.toml   # optional; defaults apply without it
podman-compose up --build -d
```

Compose starts Postgres, runs `init` once, then `serve`. Everything Aster writes lands in `./data`:

```
data/aster.toml   live config (edit it, or /set)
data/logs/        JSONL logs
data/ingest/      files for /ingest
```

Open the dashboard at http://127.0.0.1:3000 and sign in with your `ASTER_OWNER_TOKEN` ([dashboard](dashboard.md)).

Or the terminal console:

```bash
source bin/activate
aster console          # runs inside the container
```

```bash
aster-logs             # server output   (podman-compose logs -f aster)
aster-down             # stop; keeps the database volume
```

All helpers: [commands](commands.md). For a VPS behind Cloudflare: [deploy](deploy.md).

Never use `down -v` unless you want to erase Aster's database.

## native

Requires PostgreSQL and `DATABASE_URL` in `.env`.

```bash
./target/release/aster init
./target/release/aster serve
```

Then type `hello` on the dashboard or in the console. Aster knows nothing yet, so it will ask. See [training](training.md).

← [docs](README.md)
