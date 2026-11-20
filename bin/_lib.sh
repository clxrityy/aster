# shellcheck shell=bash
# Shared helpers for Aster's command bins. Sourced, not executed.
#
# Modes (ASTER_MODE, auto-detected when unset):
#   deploy  deploy/compose.yaml + deploy/.env  (a VPS set up by deploy/setup.sh)
#   local   compose.yaml + .env                (podman on your machine)
#   native  target/release/aster + .env        (no containers)

set -euo pipefail

ASTER_HOME="${ASTER_HOME:-$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)}"
export ASTER_HOME

die() {
    echo "aster: $*" >&2
    exit 1
}

say() {
    echo "aster: $*" >&2
}

aster_mode() {
    if [[ -n "${ASTER_MODE:-}" ]]; then
        echo "$ASTER_MODE"
    elif [[ -f "$ASTER_HOME/deploy/.env" ]]; then
        echo deploy
    elif [[ -f "$ASTER_HOME/.env" ]] && [[ -n "$(env_get POSTGRES_PASSWORD "$ASTER_HOME/.env")" ]]; then
        echo local
    else
        echo native
    fi
}

# The env file and compose file for the current mode.
env_file() {
    case "$(aster_mode)" in
        deploy) echo "$ASTER_HOME/deploy/.env" ;;
        *) echo "$ASTER_HOME/.env" ;;
    esac
}

compose_file() {
    case "$(aster_mode)" in
        deploy) echo "$ASTER_HOME/deploy/compose.yaml" ;;
        local) echo "$ASTER_HOME/compose.yaml" ;;
        *) die "no containers in native mode (set ASTER_MODE=local or deploy)" ;;
    esac
}

# Read KEY from a dotenv file without executing it: the last plain KEY=value
# line wins; surrounding quotes are removed. Never `source` the file.
env_get() {
    local key="$1" file="$2" line value=""
    [[ -f "$file" ]] || return 0
    while IFS= read -r line || [[ -n "$line" ]]; do
        line="${line#"${line%%[![:space:]]*}"}"
        [[ "$line" == "$key="* ]] || continue
        value="${line#*=}"
        if [[ "$value" =~ ^\"(.*)\"$ || "$value" =~ ^\'(.*)\'$ ]]; then
            value="${BASH_REMATCH[1]}"
        fi
    done <"$file"
    printf '%s' "$value"
}

# Set KEY=value in a dotenv file (replace or append), keeping it private.
env_set() {
    local key="$1" value="$2" file="$3" tmp
    [[ "$value" != *$'\n'* ]] || die "refusing a multi-line value for $key"
    tmp="$(mktemp "${file}.XXXXXX")"
    chmod 600 "$tmp"
    if [[ -f "$file" ]]; then
        awk -v key="$key" 'index($0, key "=") != 1' "$file" >"$tmp"
    fi
    printf '%s=%s\n' "$key" "$value" >>"$tmp"
    mv "$tmp" "$file"
    chmod 600 "$file"
}

owner_token() {
    local token="${ASTER_OWNER_TOKEN:-}"
    [[ -n "$token" ]] || token="$(env_get ASTER_OWNER_TOKEN "$(env_file)")"
    [[ -n "$token" ]] || die "ASTER_OWNER_TOKEN is not set in $(env_file)"
    printf '%s' "$token"
}

# Where the API answers from this machine.
api_url() {
    if [[ -n "${ASTER_URL:-}" ]]; then
        echo "$ASTER_URL"
        return
    fi
    case "$(aster_mode)" in
        deploy)
            local port
            port="$(env_get ASTER_LOCAL_PORT "$(env_file)")"
            echo "http://127.0.0.1:${port:-8080}"
            ;;
        local)
            local port
            port="$(env_get ASTER_PORT "$(env_file)")"
            echo "http://127.0.0.1:${port:-3000}"
            ;;
        *) echo "http://127.0.0.1:3000" ;;
    esac
}

# podman-compose, `podman compose`, or docker compose, with the mode's files.
compose() {
    local file envf
    file="$(compose_file)"
    envf="$(env_file)"
    if command -v podman-compose >/dev/null 2>&1; then
        podman-compose --env-file "$envf" -f "$file" "$@"
    elif command -v podman >/dev/null 2>&1 && podman compose version >/dev/null 2>&1; then
        podman compose --env-file "$envf" -f "$file" "$@"
    elif command -v docker >/dev/null 2>&1; then
        docker compose --env-file "$envf" -f "$file" "$@"
    else
        die "podman-compose (or podman compose / docker compose) is required"
    fi
}

# Is the aster service up?
running() {
    [[ "$(aster_mode)" != native ]] || return 1
    compose ps --services --filter status=running 2>/dev/null | grep -qx aster
}

# curl the API with the owner token, without putting it on the command line
# (the header comes from a process substitution, invisible to `ps`).
api() {
    local method="$1" path="$2"
    shift 2
    local token
    token="$(owner_token)"
    curl --silent --show-error --fail-with-body \
        -X "$method" \
        -H @<(printf 'Authorization: Bearer %s\n' "$token") \
        -H 'Content-Type: application/json' \
        -w '\n' \
        "$(api_url)$path" "$@"
}

# JSON string literal for a shell value.
json_string() {
    local s="$1"
    s="${s//\\/\\\\}"
    s="${s//\"/\\\"}"
    s="${s//$'\t'/\\t}"
    s="${s//$'\n'/\\n}"
    s="${s//$'\r'/\\r}"
    printf '"%s"' "$s"
}

# The folder mounted as /app/data (aster.toml, logs/, ingest/).
data_dir() {
    case "$(aster_mode)" in
        deploy) echo "$ASTER_HOME/deploy/data" ;;
        *) echo "$ASTER_HOME/data" ;;
    esac
}

backups_dir() {
    case "$(aster_mode)" in
        deploy) echo "$ASTER_HOME/deploy/backups" ;;
        *) echo "$ASTER_HOME/backups" ;;
    esac
}
