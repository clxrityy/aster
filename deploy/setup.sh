#!/usr/bin/env bash
# One-time (and re-runnable) setup of Aster on a VPS with podman.
#
#   git clone <your repo> ~/aster && cd ~/aster && deploy/setup.sh
#
# It checks prerequisites, writes deploy/.env with fresh secrets (never
# printed), asks for your dashboard hostname and Cloudflare tunnel token,
# copies a starting aster.toml, starts the stack, and can install a systemd
# user service so Aster comes back after a reboot. Safe to run again: existing
# secrets and config are kept.
#
#   deploy/setup.sh [--systemd] [--no-start]

set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."
export ASTER_MODE=deploy
# shellcheck source=bin/_lib.sh
source bin/_lib.sh

systemd=false
start=true
for arg in "$@"; do
    case "$arg" in
        --systemd) systemd=true ;;
        --no-start) start=false ;;
        -h | --help)
            sed -n '2,13p' "$0"
            exit 0
            ;;
        *) die "unknown option $arg" ;;
    esac
done

# --- prerequisites -----------------------------------------------------------
for tool in podman openssl curl git; do
    command -v "$tool" >/dev/null 2>&1 || die "$tool is required (e.g. sudo apt install $tool)"
done
if ! command -v podman-compose >/dev/null 2>&1 && ! podman compose version >/dev/null 2>&1; then
    die "podman-compose is required (e.g. sudo apt install podman-compose, or pipx install podman-compose)"
fi

envf="deploy/.env"
umask 077
touch "$envf"
chmod 600 "$envf"

# --- secrets (generated once, never shown) -------------------------------------
for key in POSTGRES_PASSWORD ASTER_OWNER_TOKEN; do
    if [[ -z "$(env_get "$key" "$envf")" ]]; then
        env_set "$key" "$(openssl rand -hex 32)" "$envf"
        say "generated $key"
    fi
done

# --- hostname and tunnel ---------------------------------------------------------
if [[ -z "$(env_get ASTER_HOSTNAME "$envf")" ]]; then
    read -r -p "Dashboard hostname (e.g. aster.example.com): " hostname
    [[ "$hostname" =~ ^[A-Za-z0-9]([A-Za-z0-9.-]*[A-Za-z0-9])?$ ]] || die "that is not a hostname"
    env_set ASTER_HOSTNAME "$hostname" "$envf"
fi
if [[ -z "$(env_get TUNNEL_TOKEN "$envf")" ]]; then
    echo "Cloudflare Zero Trust → Networks → Tunnels → Create a tunnel (cloudflared)."
    echo "Copy the token from the install command (the long string after --token)."
    read -r -s -p "Tunnel token (input hidden): " tunnel
    echo
    [[ -n "$tunnel" && "$tunnel" != *[[:space:]]* ]] || die "empty or malformed tunnel token"
    env_set TUNNEL_TOKEN "$tunnel" "$envf"
fi

# --- data and config ---------------------------------------------------------------
umask 022
mkdir -p deploy/data/ingest deploy/data/logs deploy/backups
chmod 700 deploy/backups
if [[ ! -f deploy/data/aster.toml ]]; then
    cp aster.example.toml deploy/data/aster.toml
    say "wrote deploy/data/aster.toml (edit it, or use the dashboard's Config page)"
fi

# --- start ------------------------------------------------------------------------
if $start; then
    bin/aster-up
fi

# --- start on boot -------------------------------------------------------------------
if $systemd; then
    unit_dir="$HOME/.config/systemd/user"
    mkdir -p "$unit_dir"
    compose_cmd="$(command -v podman-compose || true)"
    if [[ -n "$compose_cmd" ]]; then
        up="$compose_cmd --env-file $PWD/deploy/.env -f $PWD/deploy/compose.yaml up -d"
        down="$compose_cmd --env-file $PWD/deploy/.env -f $PWD/deploy/compose.yaml down"
    else
        up="$(command -v podman) compose --env-file $PWD/deploy/.env -f $PWD/deploy/compose.yaml up -d"
        down="$(command -v podman) compose --env-file $PWD/deploy/.env -f $PWD/deploy/compose.yaml down"
    fi
    cat >"$unit_dir/aster.service" <<UNIT
[Unit]
Description=Aster (podman compose stack)
Wants=network-online.target
After=network-online.target

[Service]
Type=oneshot
RemainAfterExit=yes
WorkingDirectory=$PWD/deploy
ExecStart=$up
ExecStop=$down
TimeoutStartSec=900

[Install]
WantedBy=default.target
UNIT
    systemctl --user daemon-reload
    systemctl --user enable aster.service
    # Let the user's services run without an open login session.
    loginctl enable-linger "$USER" 2>/dev/null || say "run 'sudo loginctl enable-linger $USER' so Aster starts at boot"
    say "installed systemd user service aster.service"
fi

hostname="$(env_get ASTER_HOSTNAME "$envf")"
cat <<DONE

Aster is set up. Remaining steps, in the Cloudflare dashboard:

  1. Your tunnel → Public hostname → add:  $hostname  →  HTTP  nginx:8080
  2. Zero Trust → Access → Applications → Self-hosted → $hostname
     Policy: Allow → Emails → your own address only.
     (Then only you can even reach the sign-in page.)

Sign in at https://$hostname with your owner token:   bin/aster-token show
Everyday commands:   source bin/activate   then   aster-status, aster-logs, aster console
DONE
