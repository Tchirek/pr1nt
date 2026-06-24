# Security policy

## Reporting a vulnerability

Please report security issues privately by opening a
[GitHub security advisory](https://github.com/Tchirek/pr1nt/security/advisories/new)
rather than a public issue. I'll try to respond within a few days.

## Handling of secrets

- Real secrets never belong in the repository. They live in the Cloudflare Worker
  secret store or in a local, git-ignored `local-server/.env`.
- `local-server/.env.example` documents every configuration key with placeholder
  values only.
- A real `.env` is never committed; `local-server/.env.example` documents every
  key with placeholder values only.

## Trust boundaries

- The public intake server (`8788`) authenticates uploads with short-lived
  tokens and a shared secret; the admin API (`8789`) is loopback-only and gated
  by a separate admin token.
- Document bytes travel directly between the browser and the local server over
  the Cloudflare Tunnel; the Worker only mints tokens and tracks job state.
