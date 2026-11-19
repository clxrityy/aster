# console

`aster console` is a client of `aster serve`. Closing it doesn't stop Aster.

```
 ✶ aster  chat  ● live  ✉ 1 waiting
┌ conversation ───────────────────┐┌ levels ─────────────┐
│ 10:00 you   hello               ││ energy   ████░░ 75↓ │
│ 10:00 aster Hi, I'm Aster. ▲    │└─────────────────────┘
│                                 │┌ inbox ──────────────┐
│                                 ││ 1. ? What is "x"?   │
│                                 │└─────────────────────┘
│                                 │┌ thought trace ──────┐
│                                 ││ reply conf .66/.40  │
└─────────────────────────────────┘└─────────────────────┘
 › _
```

| command | does |
|---|---|
| *text* | talk |
| `/train` · `/chat` | train mode asks you to rate each reply |
| `+` · `-` · `/good` · `/bad` | rate the last reply |
| `/correct <reply>` | rate it bad and teach the better reply |
| `/teach <prompt> => <reply>` | teach directly |
| `/answer <n> <text>` | answer inbox item *n* |
| `/approve <n>` · `/deny <n>` | decide a request |
| `/remember <text>` · `/forget <id>` | memories |
| `/ingest <path>` | learn a file inside `server.ingest_dir` |
| `/set <key> <value>` · `/get <key>` · `/config [section]` | live config |
| `/pause` · `/resume` | freeze/resume levels and initiative |
| `/status` · `/history` | mind stats · level changes |
| `/snapshot [why]` · `/snapshots` · `/rollback <v>` | mind versions |
| `/logs <category> [n]` | tail today's log |
| `/trace` or `Tab` · `/clear` · `/help` · `/quit` | view |

Keys: `↑/↓` history, `PgUp/PgDn` scroll, `Esc` clear, `Ctrl-C` quit.

```bash
aster console --url http://127.0.0.1:3001   # other server
```

← [docs](README.md)
