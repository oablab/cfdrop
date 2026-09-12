# cfdrop

Deploy a directory to a **temporary Cloudflare account** — no signup, no wrangler, no Node — and get a live `workers.dev` URL for browsing. Or, with `--own`, [deploy straight into your own account](#deploy-into-your-own-account---own) with a single-permission API token. Self-contained Rust CLI.

```
cfdrop deploy --directory path/to/dir/
```

```
Found 15 file(s), 42.3 KiB total.
Provisioning temporary Cloudflare account...
Solving proof-of-work (2000000 SHA-256 hashes)...
Temporary account Waiting Salmonberry (created), expires 23:41 UTC

✅ Deployed: https://my-site.waiting-salmonberry.workers.dev

This temporary account expires in ~58 minutes.
Keep it by claiming: https://dash.cloudflare.com/claim-preview?claimToken=...
```

## How it works

Implements Cloudflare's [claim-deployments (temporary accounts)](https://developers.cloudflare.com/workers/platform/claim-deployments/) provisioning API and the [static assets direct upload](https://developers.cloudflare.com/workers/static-assets/direct-upload/) protocol natively:

1. `POST /provisioning/previews/challenge` → proof-of-work challenge
2. Solve the SHA-256 checkpoint chain locally (`k × g` sequential hashes, ~1s)
3. `POST /provisioning/previews` (ToS acceptance) → temp account id + API token + claim URL
4. `POST .../assets-upload-session` with a content-hash manifest → upload JWT + buckets
5. `POST .../workers/assets/upload?base64=true` per bucket → completion JWT
6. `PUT .../workers/scripts/{name}` (assets-only Worker) + enable `workers.dev` → live URL

The temporary account is cached in the OS config dir (`~/Library/Application Support/cfdrop/state.json` on macOS, mode 0600) and reused across deploys until it expires — same behavior as `wrangler deploy --temporary`.

## Commands

| Command | Description |
|---------|-------------|
| `cfdrop deploy -d <dir> [-n name] [-y] [--fresh] [--auth user:pass] [--md]` | Bundle and deploy a directory to a temporary account |
| `cfdrop deploy -d <dir> -n name --own [--account <id>] [--force]` | Deploy into **your own** Cloudflare account (see below) |
| `cfdrop rm -n name [--account <id>] [--force]` | Delete a site from your own account |
| `cfdrop status` | Show cached temp account, claim URL, expiry; whether own-account env is set |
| `cfdrop logout` | Forget the cached temp account |

- `-y` accepts Cloudflare's Terms of Service / Privacy Policy without prompting (required for non-interactive use)
- `--fresh` forces provisioning a new account even if a cached one is still valid
- `--auth user:pass` protects the site with HTTP Basic Auth: deploys a small guard Worker in front of the assets (`run_worker_first`) that returns 401 unless the browser sends the matching credential. Note the credential is baked into the Worker script — fine for a 60-minute preview, not a real security boundary. For long-lived sites, claim the account and use [Cloudflare Access](https://developers.cloudflare.com/cloudflare-one/policies/access/) instead (not available on temporary accounts).
- `--md` treats the directory as Markdown: every `*.md` is converted (pulldown-cmark: tables, strikethrough, footnotes, task lists) into a dark-theme, mobile-first HTML page — vertical scrolling only, wide tables scroll inside their own block. Non-markdown files are copied through. Unless an `index.md`/`index.html` exists, an index page listing all pages as tappable cards is generated. Titles come from the first `# heading`.
- Worker name defaults to the sanitized directory name

## Deploy into your own account (`--own`)

Cloudflare Drop itself is anonymous-first: deploy to a throwaway account, then *claim* it
within 60 minutes if you want to keep it. When you already know the site belongs in your
account — a docs site, an internal dashboard, anything that should outlive the hour — skip
the claim step and deploy straight into it:

```bash
export CLOUDFLARE_API_TOKEN=...          # from a file or your secret manager; never on argv
cfdrop deploy -d ./site -n docs --own    # → https://docs.<your-subdomain>.workers.dev
```

No expiry, no claim URL, and the Worker shows up in your dashboard like any other. The only
prerequisite is an account API token with the `Workers Scripts: Edit` permission (next section).

### 1. Create the API token

All `--own` needs is a **Cloudflare Account API Token with one permission: Workers
Scripts · Edit**. Nothing else.

Create it at **`https://dash.cloudflare.com/YOUR_CF_ACCOUNT_ID/api-tokens`** (Account →
Manage Account → API Tokens → Create Token → Custom Token):

| Field | Value |
|---|---|
| Permissions | `Account` · `Workers Scripts` · `Edit` — that is the whole list |
| Zone Resources | leave empty (cfdrop never touches zones) |
| TTL | give it an end date; rotate rather than keep one forever |

Do **not** use the "Edit Cloudflare Workers" template — it adds zone routes, account
settings and user-details permissions cfdrop does not need. Do not use the Global API Key.

Why an *account* token rather than one from your profile: it belongs to the account, not to
you, so it keeps working when memberships change (right for CI), and it is scoped to that one
account by construction — there is no "Account Resources" step to get wrong. A *User* API
token (Profile → API Tokens) with the same single permission works too, if you scope its
Account Resources to the target account. cfdrop's preflight only calls endpoints that accept
both kinds (`GET /accounts`, `GET /accounts/{id}/workers/subdomain`); it never calls
`/user/*`, where account tokens answer `Invalid API Token` even when perfectly valid.

Check the token before the first deploy (values stay in your shell):

```bash
# who am I → should list exactly the account you created it in
curl -s -H "Authorization: Bearer $CLOUDFLARE_API_TOKEN" \
  https://api.cloudflare.com/client/v4/accounts | jq '.result[] | {id, name}'

# can I see Workers? → success:true and your workers.dev subdomain
ACC=<account id from above>
curl -s -H "Authorization: Bearer $CLOUDFLARE_API_TOKEN" \
  https://api.cloudflare.com/client/v4/accounts/$ACC/workers/subdomain | jq
```

If the second call answers `{"code":10000,"message":"Authentication error"}` the token is
alive but missing `Workers Scripts: Edit` — the most common mistake. (Do not test with
`/user/tokens/verify`: an account token fails there by design.)

### 2. Pick the account

| You have | Do |
|---|---|
| A token that sees exactly one account | `cfdrop deploy -d ./site -n docs --own` — the account is inferred |
| A token that sees several accounts | `cfdrop deploy -d ./site -n docs --account <id>` |
| A fixed target for every deploy | `export CLOUDFLARE_ACCOUNT_ID=<id>` — then plain `cfdrop deploy -d ./site -n docs` goes to your account |
| Token in a file rather than the env | add `--token-file ~/.config/cloudflare/cfdrop.token` |

The account id is the 32-hex string in dashboard URLs (`dash.cloudflare.com/<account id>/…`)
and in the `GET /accounts` output above.

**Own mode is always explicit.** `--own`, `--account`, or `CLOUDFLARE_ACCOUNT_ID` switch it
on. A bare `CLOUDFLARE_API_TOKEN` in the environment — which many shells carry for wrangler —
does *not*, so a throwaway preview cannot land in a real account by accident. `--temporary`
forces the default even with `CLOUDFLARE_ACCOUNT_ID` set. If own mode is requested but no
token can be found, cfdrop stops with an error; it never falls back to a temporary account.

### 3. Deploy, update, delete

```bash
cfdrop deploy -d ./site -n docs --own          # first deploy
cfdrop deploy -d ./site -n docs --own          # same name = update in place, same URL
cfdrop rm -n docs                              # delete the Worker (own account only)
```

Every cfdrop deploy tags its Worker `cfdrop`. A `--name` that collides with an existing
Worker **without** that tag is refused — a typo cannot overwrite the API you hand-wrote last
year. `--force` overrides, for both `deploy` and `rm`. Temporary-account sites never need
`rm`; they expire on their own.

Your account must already have a workers.dev subdomain (temporary accounts get one
automatically). If it does not, the deploy stops with the exact `PUT` to register one.

### 4. From CI

```yaml
# .github/workflows/docs.yml
- name: Deploy docs to Cloudflare
  env:
    CLOUDFLARE_API_TOKEN: ${{ secrets.CLOUDFLARE_API_TOKEN }}   # Workers Scripts: Edit only
    CLOUDFLARE_ACCOUNT_ID: ${{ vars.CLOUDFLARE_ACCOUNT_ID }}
  run: |
    curl -sL https://github.com/oablab/cfdrop/releases/latest/download/cfdrop-linux-amd64.tar.gz | tar xz
    ./cfdrop deploy -d ./public -n docs
```

Use the account token from step 1 as the secret: it does not stop working when the person
who created it leaves the account, and it cannot reach anything but Workers in that one account.

### Troubleshooting

| Message | Cause | Fix |
|---|---|---|
| `Cloudflare rejected the API token` | token revoked, mistyped, or pasted with a trailing character | recreate; check `curl …/accounts` as above |
| `needs the Workers Scripts: Edit permission` | token exists but the permission group is missing | edit the token, add `Account · Workers Scripts · Edit` |
| `the token can see N accounts; pick one with --account` | user token spans several accounts | `--account <id>` or `CLOUDFLARE_ACCOUNT_ID` |
| `the token has no access to account X` | `--account` names an account outside the token's scope | fix the id, or widen Account Resources on the token |
| `already exists … and was not deployed by cfdrop` | `--name` collides with a Worker you made elsewhere | choose another name, or `--force` if you really mean it |
| `no workers.dev subdomain registered` | fresh account, subdomain never chosen | register once (dashboard or the printed `PUT`) |
| `Invalid API Token` from `/user/tokens/verify` in your own scripts | it is an account-owned token | use `GET /accounts/{id}/tokens/verify` instead; cfdrop already does |

`--auth user:pass` still works in own mode but prints a warning: the credential is baked into
the Worker script, which is fine for an hour-long preview and wrong for a permanent site. Put
[Cloudflare Access](https://developers.cloudflare.com/cloudflare-one/policies/access/) in
front of the workers.dev hostname instead.

## `--md` on a phone

Four `.md` files, one deploy — mermaid diagrams, syntax-highlighted code, task lists (source: [`examples/md-sample/`](examples/md-sample/)):

| Mermaid `graph TD` | Mermaid `sequenceDiagram` | Syntax highlighting | Tables & task lists |
|---|---|---|---|
| ![mermaid flow](docs/screenshots/md-mermaid-flow.png) | ![mermaid sequence](docs/screenshots/md-mermaid-sequence.png) | ![highlighted rust](docs/screenshots/md-syntax-highlight.png) | ![quick start](docs/screenshots/md-quick-start.png) |

## Notes & limits

- Temporary accounts last **60 minutes** unless claimed via the printed claim URL; unclaimed accounts and their deployments auto-delete
- Asset limits on temp accounts: ≤ 1,000 files, ≤ 5 MiB per file
- Hidden files/dirs (`.git`, `.DS_Store`, ...) are skipped
- The claim URL is a **bearer credential** — anyone holding it can claim the account. The state file is written with mode 0600; don't share it
- Asset hash scheme (must match the server): `hex(sha256(base64(content) + extension))[..32]`
- The Cloudflare API returns `"errors": null` / `"buckets": null` (explicit nulls) — the envelope types tolerate this

## Build

```
cargo test
cargo build --release
```

Single static-ish binary (rustls, no OpenSSL dependency).

## Example

`examples/gen-triage-site.py` fetches all open issues from `openabdev/openab` via the `gh` CLI and generates a mobile-first static site (index tiles + per-issue detail pages with summary, current-vs-expected flow diagram, root-cause analysis, and a suggested triage response), then:

```
python3 examples/gen-triage-site.py
cfdrop deploy --directory /tmp/openab-issues-site --name openab-issues -y
```

Detail-page analysis sections are read from `/tmp/openab-analysis/<number>.json` when present (e.g. produced by an AI agent pass over the issues); pages render fine without them. Point it at another repo by editing `REPO` at the top of the script.
