# Quick start

## Install

Grab a binary from the GitHub release (`linux-amd64`, `linux-arm64`, `macos-arm64`):

```bash
tar xzf cftmp-macos-arm64.tar.gz
mv cftmp ~/.local/bin/
```

## First deploy

```bash
cftmp deploy --directory ./my-site -y
```

- [x] `-y` accepts Cloudflare's ToS (required non-interactively)
- [x] URL printed on success
- [ ] Claim the account if you want to keep it past 60 minutes

## Everyday commands

| Command | What it does |
|---------|--------------|
| `cftmp deploy -d dir/ -y` | deploy a directory |
| `cftmp deploy -d dir/ --md -y` | convert Markdown → mobile HTML, then deploy |
| `cftmp status` | show cached account + claim URL |
| `cftmp logout` | forget the cached account |
