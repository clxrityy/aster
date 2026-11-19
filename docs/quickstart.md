# quickstart

Requires PostgreSQL and a filled `.env` (`DATABASE_URL`, `ASTER_OWNER_TOKEN`). See the root README for setup.

```bash
cargo build --release -p aster-api
cp aster.example.toml aster.toml   # optional; defaults apply without it
```

```bash
./target/release/aster init        # once
./target/release/aster serve       # keeps running; Aster lives here
```

In a second terminal:

```bash
./target/release/aster console
```

Then type `hello`. Aster knows nothing yet, so it will ask. See [training](training.md).

← [docs](README.md)
