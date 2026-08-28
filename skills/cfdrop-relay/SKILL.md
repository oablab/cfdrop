---
name: cfdrop-relay
description: Push a freshly deployed cfdrop URL to a cfdrop relay so a paired viewer device (e.g. an iPad running the cfdrop viewer app) opens it automatically. Use when the user says "cfdrop and open on the viewer", "push to the relay", "show it on the iPad", or wants a deployed site to appear on a paired screen without sharing the URL manually.
---

# cfdrop-relay — Auto-open Deploys on a Paired Viewer

Extends the `cfdrop` skill: after a successful deploy, `--notify` (cfdrop ≥0.6.0)
pushes the live URL to a relay server, and any viewer device paired with that relay
opens the site immediately. The relay server and viewer app live in the
`cfdrop-app` repo.

## Prerequisites

- `cfdrop` ≥0.6.0 (`cfdrop --version`)
- A running cfdrop relay (default port 8788) reachable from this machine — referred
  to below as `relay-host`
- The relay token, stored on the relay host at `~/.cfdrop-relay-token`

## Usage

```bash
export CFDROP_NOTIFY=http://relay-host:8788   # relay endpoint
export CFDROP_RELAY_TOKEN=$(ssh relay-host 'cat ~/.cfdrop-relay-token')

cfdrop deploy --directory /tmp/<slug>-site --name <slug> -y --notify
```

Everything else (site generation, mobile design rules, verification) follows the
base `cfdrop` skill — `--notify` is purely additive.

## Behavior

1. cfdrop deploys as usual and prints the live URL.
2. It warms up the site first (probes `<url>/?cfdrop-warmup=N` with retries) so the
   viewer doesn't land on a cold edge cache.
3. It POSTs the URL to `CFDROP_NOTIFY` with the token in an `x-relay-token` header.
4. On success it prints `Pushed URL to relay at <endpoint>.`

## Failure modes

- **Notify failure never fails the deploy** — the site is live either way; cfdrop
  prints `warning: relay notify failed: ...` and exits 0. Re-share the URL manually.
- `CFDROP_RELAY_TOKEN must be set for --notify` — export the token (see Usage).
- `relay returned 401/403` — token mismatch; re-read it from
  `relay-host:~/.cfdrop-relay-token`.
- `warning: site not reachable yet, skipping notify` — warm-up probes never got a
  200; the URL is still printed, push it again once the site responds (redeploying
  with `--notify` is idempotent).
- Connection refused — relay not running on `relay-host`, or wrong `CFDROP_NOTIFY`.
