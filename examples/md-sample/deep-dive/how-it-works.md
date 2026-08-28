# How it works

## 1. Provisioning (proof-of-work)

Cloudflare's `provisioning/previews` API gates temp accounts behind a
SHA-256 checkpoint chain — `k × g` sequential hashes (~2M, about a second):

```rust
let mut hash = sha256(&seed);
for _segment in 0..k {
    for _ in 0..g { hash = sha256(&hash); }
    checkpoints.extend_from_slice(&hash);
}
```

The response carries the account id, a scoped `apiToken`, and the claim URL.

## 2. Asset upload

Files are announced in a manifest keyed by content hash:

```text
hash = hex(sha256(base64(content) + extension))[..32]
```

The server replies with *buckets* — only hashes it doesn't already have —
so redeploys upload just the changed files.

## 3. The Worker

Without `--auth`, the deployment is **assets-only** (no JavaScript at all).
With `--auth user:pass`, a tiny guard module runs first (`run_worker_first`)
and falls through to `env.ASSETS.fetch(request)` on a correct credential.

> Gotcha: the API returns `"errors": null` — envelope types must tolerate
> explicit nulls, not just missing fields.
