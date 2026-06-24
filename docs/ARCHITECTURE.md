# Architecture & operations

pr1nt is a small self-hosted print-intake system for a single self-service print
point (referred to here as **Room 101**). Students upload a document from their
phone or laptop, preview it, pay, and watch the print job's status; a Windows PC
next to the printer does the document conversion and the actual printing.

## The three tiers

```text
Browser
  -> Cloudflare Worker / Next.js   (web/)        public site, tRPC API, KV state
  -> Cloudflare Tunnel
  -> local-server (Windows, Rust)  (local-server/) conversion, printing, status WS
  -> SumatraPDF / printer
```

| Component | Responsibility |
|-----------|----------------|
| `web/` | Public site, tRPC API, reads/writes Cloudflare KV, issues short-lived upload tokens, tracks the queue and job state. |
| `local-server/` | Receives documents/PDFs, converts via LibreOffice/WPS/Office, prints via SumatraPDF, broadcasts status over WebSocket, serves the local admin API. |
| `local-admin/` | Loopback-only admin UI: pricing, payment QR, notice, printers, converter, queue, link diagnostics. |
| `local-app/` | Optional Electron tray wrapper that supervises the local server on the printer PC. |

## Upload paths

The Worker no longer relays large files. Document bytes go straight to the
local server over the tunnel; the Worker only mints tokens, registers jobs, and
keeps KV state.

- **Preview conversion** — browser fetches a short-lived token from the Worker,
  uploads the document directly to `/api/convert-preview`, the local server
  converts it to PDF, caches it as `print-spool/preview-cache-<id>.pdf` for two
  hours, and returns the PDF plus an `x-preview-cache-id` header.
- **Final print (non-PDF)** — the browser submits only the cached preview id, so
  the converted PDF is reused and never re-uploaded.
- **Final print (PDF)** — the browser uploads the PDF directly to `/api/print`.

The legacy Worker relay routes (`/api/convert-preview`, `/api/upload/[jobId]`)
return `410 Gone`.

## Ports

| Port | Surface |
|------|---------|
| `8788` | Public intake: `POST /api/print`, `POST /api/convert-preview`, `GET /ws/status`. Exposed through the tunnel. |
| `8789` | Local admin API + static admin UI. Loopback only. |

A Cloudflare Tunnel exposes `http://127.0.0.1:8788` at a domain such as
`https://print-api.example.com`, with the status socket at
`wss://print-api.example.com/ws/status`.

## Deployment

### Web tier

`web/` is Next.js + tRPC built with OpenNext onto a Cloudflare Worker (not the
old Pages SSR route). See [../web/DEPLOY-CLOUDFLARE.md](../web/DEPLOY-CLOUDFLARE.md).
At minimum these Worker variables must be present, plus the `PRINT_KV` binding:

```text
LOCAL_SERVER_BASE_URL          NEXT_PUBLIC_PRINT_STATUS_WS_URL
PRINT_SHARED_SECRET            UPLOAD_SIGNING_SECRET           ADMIN_TOKEN
DEFAULT_BW_PRICE               DEFAULT_COLOR_PRICE
DEFAULT_ALIPAY_QR              DEFAULT_WECHAT_QR               DEFAULT_NOTICE_MARKDOWN
DEFAULT_BW_PRINTER             DEFAULT_COLOR_PRINTER
```

If a Worker variable is missing the API fails fast with an explicit message such
as `Cloudflare Worker variable LOCAL_SERVER_BASE_URL is missing.` rather than a
bare runtime error — usually it means the variable was never set, was set without
a redeploy, or was set in the wrong environment.

### Printer PC

1. Build the local server (`cargo build --release` in `local-server/`) and place
   `SumatraPDF.exe` per [../local-server/bin/README.md](../local-server/bin/README.md).
2. Copy `local-server/.env.example` to `.env` and fill in the values.
3. Start the server (`start-local-server.bat` or the built binary).
4. Open the admin UI at `http://127.0.0.1:8789/admin`.

See [../local-server/README-DEPLOY.txt](../local-server/README-DEPLOY.txt) for the
packaged-deployment notes.

## Troubleshooting

- **Job stuck in "downloading"** — check the tunnel is up and the local server is
  reachable at `8788`; the admin UI's link diagnostics page probes each hop.
- **Print fails immediately** — confirm `SUMATRA_PDF_PATH` resolves and the
  configured printer names match `wmic printer get name`.
- **Config not syncing** — the Cloudflare KV variables are optional; if they are
  blank the server logs that remote config sync is disabled and uses local
  defaults.

## Secrets

Secrets live in the Worker's secret store or in the local, git-ignored `.env`.
Never commit a real `.env`; the `local-server/.env.example` template documents
every key.
