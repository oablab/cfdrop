# cftmp — Field Guide

Deploy any directory to a **temporary Cloudflare account** and get a live
`workers.dev` URL in seconds. No signup, no wrangler, no Node.

## Pages

- [Quick start](/quick-start) — install and first deploy
- [How it works](/deep-dive/how-it-works) — provisioning, proof-of-work, upload protocol
- [Recipes](/recipes) — auth, markdown sites, account lifecycle

## At a glance

| | |
|---|---|
| Account lifetime | 60 minutes (claimable) |
| Max files | 1,000 |
| Max file size | 5 MiB |
| Dependencies | none — single static binary |

> The claim URL printed on deploy is a **bearer credential** — whoever opens
> it owns the account. Share it only with yourself.
