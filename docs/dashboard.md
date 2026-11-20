# dashboard

A web version of the console, served by `aster serve` on the same port as the API. Open it in a browser:

```
http://127.0.0.1:3000          # local
https://aster.example.com      # on a VPS, see deploy
```

Sign in with your owner token (`aster-token show`). The browser keeps only a session cookie, never the token.

| page | does |
|---|---|
| Talk | chat or train mode, rate ▲ ▼, correct, reply to an earlier message, "Why?" opens the thought trace |
| Inbox | answer Aster's questions, approve or deny its requests |
| Mind | levels now and over time, runtime state, what Aster knows |
| Neural | engine switch, training steps, loss curve (live) |
| Memories | remember and forget |
| Teach & ingest | teach a reply, ingest a file from `data/ingest` |
| Config | every `aster.toml` key, validated before it applies |
| Snapshots | save, list, roll back |
| Logs | newest entries of each log category |

Live updates (levels arrive every tick) patch the page in place, so a half-typed message or correction is never cleared. A correction draft is kept if you open another one and come back; Esc or Cancel discards it. Messages appear as soon as you send them, and ratings as soon as you click.

The translucent panels follow the system light or dark setting. With "reduce transparency" or "reduce motion" turned on, they become solid and stop animating.

## steering a reply

"Why?" on any reply shows every candidate Aster considered, with its score and source. From there:

| button | does |
|---|---|
| **Use this** | make that candidate the reply for this prompt from now on |
| ▲ · ▼ | rate one candidate, chosen or not |
| **Not relevant here** | a recalled memory didn't belong with this prompt |
| **Reconsider this reply** | ask for a different reply to the same message |

Each one is recorded under "Your signals on this reply". The original reply stays as it was. See [training](training.md#steering).

## sessions and security

- The session cookie is `HttpOnly`, `SameSite=Strict` and `Secure`. It ends after `dashboard.session_idle_minutes` without use, after `dashboard.session_max_hours` regardless, on sign-out, or when the server restarts.
- Changes from the browser also need a per-session CSRF token. The page handles that.
- After `dashboard.login_max_failures` wrong tokens, each further sign-in waits longer, up to 15 minutes.
- Every page is sent with a strict Content-Security-Policy and can't be framed.
- `cookie_secure = true` is right behind HTTPS and on `http://localhost` in Chrome and Firefox. If sign-in keeps returning to the sign-in page on a local plain-HTTP setup (Safari may refuse `Secure` cookies there), set `dashboard.cookie_secure = false` for that machine only. Never turn it off for a public host.

Turn the dashboard off with `dashboard.enabled = false`. The API and console keep working.

← [docs](README.md)
