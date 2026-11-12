# aster

local-first Rust/PostgreSQL foundation with persistent identity, an initial regulatory snapshot, and a read-only owner API.

- [native quickstart](#native-quickstart)
    - for running natively on your machine
- [local containers](#local-containers)
    - for running with Docker
- [HTTP interface](#http-interface-read-only)
    - for accessing the HTTP API in read-only mode

### prerequisites

- [Rust](https://www.rust-lang.org/) (`1.99.1`)
- [PostgreSQL](https://www.postgresql.org/)
- [OpenSSL](https://www.openssl.org/)

## native quickstart

- initialize PostgreSQL database
    1. create a new role
    2. enter the password interactively
    3. create a new database owned by the role

```bash
psql postgres
```

```sql
CREATE ROLE aster LOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE;
\password aster
CREATE DATABASE aster OWNER aster;
```

- generate your credentials
    1. owner token
    2. database password

```bash
# generate owner token
openssl rand -hex 32
# generate database password
openssl rand -hex 16
```

- fill the credentials into root `.env` (use [`.env.example`](.env.example) as the field guide)
    1. **`DATABASE_URL`** -> connection URL for local `aster` database
    2. **`ASTER_OWNER_TOKEN`** -> 64-character token
    3. restrict `.env` permissions to your account
       ```bash
       chmod 600 .env
       ```
- build, check config, initialize, serve, and verify the status
    - stop the server with <kbd>Ctrl</kbd>+<kbd>C</kbd>

```bash
cargo build --locked --release -p aster-api
./target/release/aster check-config
./target/release/aster init
./target/release/aster serve
```

```bash
./target/release/aster status
```

## local containers

> requires [Docker](https://www.docker.com/) and [Docker Compose](https://docs.docker.com/compose/)

- manually set `.env`
    - **`POSTGRES_PASSWORD`** -> at least 32 random hex characters
    - **`ASTER_OWNER_TOKEN`**
- build the docker container as the owner

    ```bash
    docker compose up --build -d
    ```

    <details>
    <summary>details</summary>
    - starts PostgreSQL
    - waits for database health
    - runs `aster-init` once
    - starts the API only after init succeeds

    > both published ports bind to host `127.0.0.1`
    > optional `POSTGRES_PORT` & `ASTER_PORT` avoid conflicts with other services on the host
    </details>

- inspect with `docker compose ps`
- stop with `docker compose down`

## HTTP interface (read-only)

| endpoint | access | meaning |
|----------|--------|---------|
| `/health/live` | public | process reponds (doesn't check postgresql) |
| `/health/ready` | owner bearer token | database reachable; identity/state & initialization audit |
| `/v1/identity` | owner bearer token | protected & evolving identity snapshot |
| `/v1/state` | owner bearer token | initial regulatory state & timestamp |
| `/v1/status` | owner bearer token | readiness, version, phase, runtime/AI/browsing `false` |
| `/v1/policy` | owner bearer token | autonomous actor's rules (not a per-actor report) |