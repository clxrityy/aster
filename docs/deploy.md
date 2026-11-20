# deploy

Aster on a VPS, reachable only as your admin dashboard on one subdomain:

```
browser ─► Cloudflare Access ─► tunnel ─► cloudflared ─► nginx ─► aster ─► postgres
```

Nothing listens on a public port. cloudflared dials out to Cloudflare. nginx is published on `127.0.0.1` only. The database network has no route out.

## you need

- a VPS with `podman`, `podman-compose`, `git`, `openssl` and `curl` (Debian/Ubuntu: `sudo apt install podman podman-compose git openssl curl`)
- a domain on Cloudflare (free plan is fine)

## 1. create a tunnel

Cloudflare dashboard → Zero Trust → Networks → Tunnels → **Create a tunnel** → Cloudflared. Name it `aster`. Copy the token from the install command (the long string after `--token`). Don't run the install command; the stack runs cloudflared itself.

## 2. set up

```bash
git clone <your repo> ~/aster
cd ~/aster
deploy/setup.sh --systemd
```

It asks for your hostname (`aster.example.com`) and the tunnel token. Then it:

- writes `deploy/.env` (mode 600) with a fresh database password and owner token. They are never printed.
- copies `aster.example.toml` to `deploy/data/aster.toml`
- builds the image (with the neural engine) and starts the stack
- with `--systemd`, installs a user service so Aster comes back after a reboot

Running it again keeps existing secrets and config.

## 3. route and lock the hostname

In the tunnel's settings → **Public hostname** → add `aster.example.com`, service `HTTP`, URL `nginx:8080`.

Then Zero Trust → Access → Applications → **Add** → Self-hosted, for `aster.example.com`. Add a policy: Allow → Emails → your address. Now only you can reach even the sign-in page. Aster's own sign-in is the second lock.

## 4. sign in

```bash
bin/aster-token show
```

Open `https://aster.example.com` and paste it. See [dashboard](dashboard.md).

## day to day

```bash
source bin/activate
aster-status                 # what's running, is Aster live
aster-logs                   # follow server output
aster-logs thoughts 20       # Aster's own logs
aster console                # the terminal console, inside the container
aster-backup                 # database + config into deploy/backups/
aster-update                 # backup, git pull, rebuild, restart
```

All commands: [commands](commands.md).

From your laptop, without the browser:

```bash
bin/aster --remote you@vps console          # console over SSH
ssh -L 8080:127.0.0.1:8080 you@vps          # then http://localhost:8080
```

## files on the VPS

```
deploy/.env               secrets (600); edited by setup.sh and aster-token
deploy/data/aster.toml    live config
deploy/data/logs/         JSONL logs
deploy/data/ingest/       files to ingest (copy with scp)
deploy/backups/           aster-backup output (700)
```

The database lives in the `aster-deploy_postgres_data` podman volume. `aster-down` keeps it. It refuses `-v`.

## when something's off

| symptom | look at |
|---|---|
| Cloudflare error 502 | `aster-logs nginx`, `aster-logs aster` |
| tunnel shows down | `aster-logs cloudflared`; is `TUNNEL_TOKEN` right? |
| "That token is not right" | `aster-token show`; after `aster-token rotate`, sign in again |
| "Too many attempts" | wait; each failure doubles the wait (max 15 min) |
| live dot stays red | something buffers `/v1/events`; the bundled nginx config doesn't |

← [docs](README.md)
