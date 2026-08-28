# Quick start

## Install

Grab a binary from the GitHub release (`linux-amd64`, `linux-arm64`, `macos-arm64`):

```bash
tar xzf cfdrop-macos-arm64.tar.gz
mv cfdrop ~/.local/bin/
```

## First deploy

```bash
cfdrop deploy --directory ./my-site -y
```

- [x] `-y` accepts Cloudflare's ToS (required non-interactively)
- [x] URL printed on success
- [ ] Claim the account if you want to keep it past 60 minutes

## Everyday commands

| Command | What it does |
|---------|--------------|
| `cfdrop deploy -d dir/ -y` | deploy a directory |
| `cfdrop deploy -d dir/ --md -y` | convert Markdown → mobile HTML, then deploy |
| `cfdrop status` | show cached account + claim URL |
| `cfdrop logout` | forget the cached account |
