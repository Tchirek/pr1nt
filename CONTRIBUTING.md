# Contributing to pr1nt

Thanks for taking a look. This is a small, self-hosted project, but patches and
issues are welcome.

## Repository layout

```text
web/           Cloudflare Worker / Next.js intake app
local-server/  Rust (Axum) local print server
local-admin/   loopback admin UI
local-app/     optional Electron tray wrapper
cloudflare/    D1 schema and R2 policy templates
dpj/           small document helper
scripts/       repository safety checks
docs/          architecture and operations notes
```

See [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) for how the pieces fit together.

## Local checks

Run the same checks CI runs before opening a pull request.

**Rust (`local-server/`):**

```bash
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
cargo test
```

**Web (`web/`):**

```bash
npm ci
npm run typecheck
```

## Conventions

- Keep the `local-server` modules cohesive — handlers in `handlers`, domain types
  in `model`, error mapping in `error`, and so on. Avoid growing a single file.
- Add a unit test alongside any non-trivial pure function.
- Never commit a real `.env`; update `local-server/.env.example` instead when you
  add a configuration key.

## Reporting security issues

Please see [SECURITY.md](SECURITY.md) — do not open a public issue for
vulnerabilities.
