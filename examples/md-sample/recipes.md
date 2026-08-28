# Recipes

## Password-protect a preview

```bash
cftmp deploy -d site/ --auth alice:s3cret -y
```

Preview-grade only — the credential is baked into the Worker script.
For long-lived sites, claim the account and use Cloudflare Access.

## Publish notes as a mobile site

```bash
cftmp deploy -d notes/ --md -y
```

Every `*.md` becomes a dark-theme mobile page. This very site is the demo:
tables, ~~horizontal scrolling~~ *vertical* scrolling, `inline code`, and
fenced blocks all render.

## Account lifecycle

1. First deploy provisions and caches the account
2. Later deploys reuse it — same `--name`, same URL
3. `--fresh` forces a new account (new subdomain!)
4. Expired? Next deploy provisions automatically — re-share the new URL
