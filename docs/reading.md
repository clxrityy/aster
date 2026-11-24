# reading

Aster can read web pages and files you hand it. It keeps the links it finds and may ask to read one of them later. It never fetches anything on its own: a page is read only when you send it or approve Aster's request.

## send it something

| | dashboard (Teach & read) | console | API |
|---|---|---|---|
| a web page | Read a web page | `/read <url>` | `POST /v1/read` `{"url"}` |
| a file on your machine | drop it, or click to choose | `/upload ~/notes.md` | `POST /v1/ingest/upload` `{"name","text"}` |
| some text | Paste text | `/paste notes` … then a line with just `.` | same as a file |
| a file already on the server | Ingest a file | `/ingest <path>` | `POST /v1/ingest` `{"path"}` |

- **Pages** are read down to their text: paragraphs, headings, list items. Menus, headers, footers and scripts are skipped. The text is saved as `data/ingest/web/<site>-<path>-<hash>.txt` (so `aster-backup` keeps it), then learned like an ingested file: vocabulary, the language model, and one recall passage per paragraph. Up to 100 links are kept.
- **Files** can be `.txt`, `.md`, `.markdown`, `.aster` or `.html`, up to `server.ingest_max_bytes`. They are saved in `data/ingest/uploads/` and ingested. HTML is read down to its text first, and `.aster` files load as [packs](training.md#packs).
- Blank lines separate passages. Aster recalls one passage at a time, so short paragraphs recall better than one long block.

## what Aster asks for

When Aster is idle and curious enough (`cognition.initiative_curiosity_min`), it may:

- **Ask to read a link** from a page it read, when the link's text or address names a word it doesn't know. The request shows up in the inbox with the link. **Approve** reads the page, and the result shows as a notice. **Deny** reads nothing. Either way, Aster never asks about that link again.
- **Ask you to send something** about a word it asked about earlier and still doesn't know. Answer with a link and Aster reads that page. Answer with words and it learns them as the meaning, as before.

These count toward `runtime.max_outbound_per_hour` and `runtime.max_pending_outbound` like every other question or request.

## safety

- Only `http` and `https` URLs, with no user name or password in them.
- Before connecting, Aster looks up the host and refuses it if it is a private, loopback, link-local, shared (CGNAT) or reserved address. It then connects only to the addresses it checked. Every redirect is checked the same way, up to `web.max_redirects`. To read a page on your LAN, set `web.allow_private = true`.
- Only text responses are read (HTML, plain text, Markdown). Anything past `web.max_bytes` is cut off, and the whole fetch has `web.timeout_secs`.
- What is logged: the audit log records the host, size and origin (`owner`, `approved`, `answer`), and the page text stays in `data/ingest/web/`.
- `web.enabled = false` turns all of this off.

## settings (`[web]`)

| key | default | meaning |
|---|---|---|
| `enabled` | true | read pages at all |
| `timeout_secs` | 15 | whole fetch, redirects included |
| `max_bytes` | 2097152 | longer pages are cut off |
| `max_redirects` | 3 | |
| `allow_private` | false | allow private and local addresses |

← [docs](README.md)
