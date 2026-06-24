# pr1nt

pr1nt is a small self-hosted print intake system. The public app runs on
Cloudflare Workers/OpenNext; the Windows local server pulls print work over
outbound HTTPS, prepares documents locally, and reports status back to the
Worker.

## Repository

```text
web/           Cloudflare Worker/OpenNext web app
local-server/  Rust local print server
local-admin/   local configuration UI
cloudflare/    D1 schema and R2 policy templates
scripts/       repository safety checks
```

## Public Scope

This repository is the public source export for the deployed system. Private
operator runbooks, local machine paths, generated print spools, packaged
Windows binaries, and live secrets are intentionally omitted.

## Development

```powershell
cd web
npm install
npm run typecheck
npm run cf:build

cd ..\local-server
cargo check
```

Cloudflare resources are described in [web/DEPLOY-CLOUDFLARE.md](./web/DEPLOY-CLOUDFLARE.md).
Secrets belong in Cloudflare secrets or ignored local `.env` files, never in
Git.
