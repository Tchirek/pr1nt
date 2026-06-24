# Cloudflare deployment

This document covers the public Cloudflare resource shape. Private operational
runbooks and machine-specific notes are not part of the public source export.

This app is deployed to Cloudflare Workers with OpenNext.

## Architecture

- Browser file bytes go directly to the `photohost` R2 bucket under the isolated
  `print-staging/` prefix with short-lived presigned PUT URLs.
- `PRINT_DB` D1 is the source of truth for document preparation and print-job
  state.
- `PRINT_KV` stores configuration and legacy compatibility data only.
- The Windows localserver connects outward to the Worker, claims pending work,
  receives an R2-backed streaming response from the Worker, converts, counts
  real pages, and reports status. The Worker does not buffer the document.
- Cloudflare Tunnel is not part of the new upload or print submission path.
- R2 originals are deleted after successful printing or a confirmed preparation
  failure. A seven-day lifecycle rule handles abandoned objects.

The current S3 credentials are scoped to the existing `photohost` bucket, so
609 uses a dedicated prefix instead of the otherwise preferred separate bucket.
The binding, signed object keys, and lifecycle rule are all restricted to
`print-staging/`; NormalPics image object prefixes are not cleaned by 609.

## Bindings

The checked-in `wrangler.jsonc` uses:

- KV binding: `PRINT_KV`
- R2 binding: `PRINT_STAGING`
- R2 bucket: `photohost` (`print-staging/` prefix only)
- D1 binding: `PRINT_DB`
- D1 database: `609-print`

Apply the D1 schema:

```bash
npx wrangler d1 execute 609-print --remote --file ..\cloudflare\print-schema.sql --yes
```

Apply R2 CORS and lifecycle configuration:

```bash
npx wrangler r2 bucket cors set photohost --file ..\cloudflare\r2-cors.json --force
npx wrangler r2 bucket lifecycle set photohost --file ..\cloudflare\r2-lifecycle.json --force
```

## Worker secrets

Set these with `wrangler secret put`:

- `UPLOAD_SIGNING_SECRET`
- `PRINT_SYNC_SECRET`
- `NORMALPICS_HANDOFF_SECRET`
- `R2_ACCESS_KEY_ID`
- `R2_SECRET_ACCESS_KEY`
- `PRINT_SHARED_SECRET` for legacy direct-upload compatibility

`PRINT_SYNC_SECRET` must match the Windows localserver.
`NORMALPICS_HANDOFF_SECRET` must match the NormalPics Worker secret
`PRINT_609_HANDOFF_SECRET`.

## Variables

`wrangler.jsonc` contains the non-secret R2 account and bucket names, plus the
allowed NormalPics browser origins.

Existing dashboard variables for prices, QR codes, printers, and notices remain
supported:

- `DEFAULT_BW_PRICE`
- `DEFAULT_COLOR_PRICE`
- `DEFAULT_ALIPAY_QR`
- `DEFAULT_WECHAT_QR`
- `DEFAULT_NOTICE_MARKDOWN`
- `DEFAULT_BW_PRINTER`
- `DEFAULT_COLOR_PRINTER`

`LOCAL_SERVER_BASE_URL` and `NEXT_PUBLIC_PRINT_STATUS_WS_URL` are only needed by
cached legacy pages and optional local status tools.

## Build and deploy

```bash
npm install
npm run typecheck
npm run cf:build
npm run deploy
```
