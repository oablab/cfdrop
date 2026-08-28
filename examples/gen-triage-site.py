#!/usr/bin/env python3
"""Generate a mobile-first static site summarizing GitHub issues.

Fetches open issues from REPO via the `gh` CLI, then renders:
  - index.html         tile per issue (no horizontal scroll, tap to open)
  - <number>.html      per-issue detail page: summary, current-vs-expected
                       flow diagram, root cause, suggested response

Detail-page analysis is read from ANALYSIS_DIR/<number>.json when present
(schema: number, summary, current_flow[], expected_flow[], root_cause,
suggested_response) — e.g. produced by an AI agent pass over the issues.
Pages without an analysis file still render with a link to GitHub.

Usage:
    python3 examples/gen-triage-site.py
    cfdrop deploy --directory /tmp/openab-issues-site --name openab-issues -y
"""
import json
import html
import os
import glob
import re
import subprocess
from datetime import datetime, timezone

REPO = "openabdev/openab"
QUERY = f"repo:{REPO}+is:issue+is:open"
SITE_TITLE = "openab · open issues"
ANALYSIS_DIR = "/tmp/openab-analysis"
OUT_DIR = "/tmp/openab-issues-site"

# ---------- fetch ----------
raw = subprocess.run(
    ["gh", "api", "-X", "GET", f"search/issues?q={QUERY}&per_page=100",
     "--jq", "[.items[] | {number, title, url: .html_url, author: .user.login, "
             "created: .created_at, comments, labels: [.labels[].name]}]"],
    check=True, capture_output=True, text=True,
).stdout
issues = json.loads(raw)
if not issues:
    raise SystemExit(f"no open issues found for {REPO}")

analyses = {}
for p in glob.glob(f"{ANALYSIS_DIR}/*.json"):
    a = json.load(open(p))
    analyses[a["number"]] = a

now = datetime.now(timezone.utc)


def age_days(created):
    dt = datetime.fromisoformat(created.replace("Z", "+00:00"))
    return (now - dt).days


def issue_type(labels):
    if "bug" in labels:
        return "bug"
    if "feature" in labels or "feature-request" in labels:
        return "feature"
    return "other"


def module_of(labels):
    skip = {"bug", "feature", "feature-request", "needs-triage", "p1", "p2", "p3",
            "pending-maintainer", "potential-regression"}
    mods = [l for l in labels if l not in skip]
    return mods[0] if mods else "—"


# Code-like tokens (file paths, Class::method, func(), snake_case, camelCase)
# get wrapped in <code> for mobile-readable inline code styling.
CODE_RE = re.compile(r"""(
    `[^`]+`                                                    # `backticked`
  | \b[\w./-]+\.(?:tsx?|m?js|rs|py|go|json|ya?ml|toml|md|mjs)\b  # file paths
  | \b[A-Za-z_][\w:]*::[\w:]+(?:\(\))?                         # Rust::paths
  | \b[A-Za-z_]\w*\(\)                                         # funcCalls()
  | \b[a-z]+(?:_[a-z0-9]+)+\b                                  # snake_case
  | \b[a-z]+[A-Z]\w*\b                                         # lowerCamelCase
)""", re.X)


def codify(escaped_text):
    """Wrap code-like tokens in <code>. Input must already be html-escaped."""
    def rep(m):
        t = m.group(0)
        if t.startswith("`") and t.endswith("`"):
            t = t[1:-1]
        return f"<code>{t}</code>"
    return CODE_RE.sub(rep, escaped_text)


BASE_CSS = """
:root { --bg:#0f1117; --card:#1a1d26; --fg:#e6e8ee; --dim:#8b90a0; --line:#2a2e3b;
        --orange:#ff9900; --red:#f2555a; --green:#4cc38a; --blue:#6ca0f5; }
* { box-sizing:border-box; margin:0; min-width:0; }
html, body { overflow-x:hidden; }
body { background:var(--bg); color:var(--fg); font:16px/1.5 -apple-system,BlinkMacSystemFont,"Segoe UI",Roboto,sans-serif;
       padding:16px; max-width:720px; margin:0 auto; -webkit-text-size-adjust:100%; }
a { color:var(--blue); }
h1 { font-size:1.2rem; margin-bottom:4px; overflow-wrap:anywhere; }
h1 .repo { color:var(--orange); }
.sub { color:var(--dim); font-size:.85rem; margin-bottom:16px; overflow-wrap:anywhere; }
.b { padding:1px 8px; border-radius:99px; font-size:.72rem; font-weight:600; white-space:nowrap; }
.b-bug { background:#3a1d20; color:var(--red); }
.b-feat { background:#16301f; color:var(--green); }
.b-other { background:#2a2e3b; color:var(--dim); }
.mod { font-family:ui-monospace,SFMono-Regular,Menlo,monospace; color:var(--blue); overflow-wrap:anywhere; }
footer { color:var(--dim); font-size:.75rem; text-align:center; margin:24px 0 8px; overflow-wrap:anywhere; }
"""

INDEX_CSS = BASE_CSS + """
.stats { display:grid; grid-template-columns:repeat(4,1fr); gap:8px; margin-bottom:20px; }
@media (max-width:480px) { .stats { grid-template-columns:repeat(2,1fr); } }
.stat { background:var(--card); border:1px solid var(--line); border-radius:10px; padding:12px; text-align:center; }
.stat .v { font-size:1.5rem; font-weight:700; }
.stat .k { font-size:.75rem; color:var(--dim); text-transform:uppercase; letter-spacing:.04em; }
.v.red { color:var(--red); } .v.green { color:var(--green); } .v.blue { color:var(--blue); }
.card { display:block; width:100%; background:var(--card); border:1px solid var(--line); border-radius:12px;
        padding:12px 14px; margin-bottom:10px; text-decoration:none; color:var(--fg); }
.card:active { background:#232734; }
.row1 { display:flex; flex-wrap:wrap; align-items:center; gap:6px 8px; font-size:.8rem; margin-bottom:6px; }
.num { color:var(--dim); font-variant-numeric:tabular-nums; }
.meta { margin-left:auto; color:var(--dim); }
.title { font-size:.95rem; font-weight:600; margin-bottom:6px; overflow-wrap:anywhere; }
.row2 { display:flex; flex-wrap:wrap; gap:4px 10px; font-size:.78rem; color:var(--dim); }
.author { overflow-wrap:anywhere; }
"""

DETAIL_CSS = BASE_CSS + """
.back { display:inline-block; margin-bottom:12px; font-size:.85rem; text-decoration:none; }
.hdr { background:var(--card); border:1px solid var(--line); border-radius:12px; padding:14px; margin-bottom:16px; }
.hdr .row1 { display:flex; flex-wrap:wrap; align-items:center; gap:6px 8px; font-size:.8rem; margin-bottom:8px; }
.num { color:var(--dim); }
.title { font-size:1.05rem; font-weight:700; overflow-wrap:anywhere; margin-bottom:8px; }
.ghlink { font-size:.85rem; }
section { margin-bottom:20px; }
h2 { font-size:.8rem; text-transform:uppercase; letter-spacing:.06em; color:var(--dim); margin-bottom:10px;
     border-bottom:1px solid var(--line); padding-bottom:6px; }
.prose { background:var(--card); border:1px solid var(--line); border-radius:12px; padding:14px;
         font-size:.92rem; overflow-wrap:anywhere; white-space:pre-wrap; }
.prose code, .step code { background:#232734; border:1px solid var(--line); border-radius:5px;
       padding:0 4px; font:.85em ui-monospace,SFMono-Regular,Menlo,monospace;
       color:var(--blue); overflow-wrap:anywhere; }
.flowwrap { display:grid; grid-template-columns:1fr 1fr; gap:12px; }
@media (max-width:560px) { .flowwrap { grid-template-columns:1fr; } }
.flow { background:var(--card); border:1px solid var(--line); border-radius:12px; padding:12px; }
.flow h3 { font-size:.78rem; text-transform:uppercase; letter-spacing:.05em; text-align:center; margin-bottom:10px; }
.flow.cur h3 { color:var(--red); } .flow.exp h3 { color:var(--green); }
.step { border-radius:8px; padding:8px 10px; font-size:.82rem; text-align:center; overflow-wrap:anywhere; }
.flow.cur .step { background:#2a1a1c; border:1px solid #4a2a2e; }
.flow.exp .step { background:#14251c; border:1px solid #23483a; }
.arrow { text-align:center; color:var(--dim); font-size:.9rem; line-height:1.6; }
.rc { font-size:.88rem; }
.resp { position:relative; }
.copybtn { position:absolute; top:10px; right:10px; background:var(--line); color:var(--fg); border:0;
           border-radius:6px; padding:4px 10px; font-size:.75rem; cursor:pointer; }
"""


def badge_bits(it):
    t = issue_type(it["labels"])
    badge = {"bug": "b-bug", "feature": "b-feat", "other": "b-other"}[t]
    label_txt = {"bug": "bug", "feature": "feature", "other": "untyped"}[t]
    return t, badge, label_txt


def flow_html(steps, cls, label):
    parts = [f'<div class="flow {cls}"><h3>{label}</h3>']
    for i, s in enumerate(steps):
        if i:
            parts.append('<div class="arrow">↓</div>')
        parts.append(f'<div class="step">{codify(html.escape(s))}</div>')
    parts.append("</div>")
    return "".join(parts)


os.makedirs(OUT_DIR, exist_ok=True)

# ---------- detail pages ----------
for it in issues:
    n = it["number"]
    a = analyses.get(n)
    _, badge, label_txt = badge_bits(it)

    if a:
        body_sections = f"""
<section><h2>1 · Summary</h2><div class="prose">{codify(html.escape(a['summary']))}</div></section>
<section><h2>2 · Current vs Expected</h2>
<div class="flowwrap">
{flow_html(a['current_flow'], 'cur', 'Current behavior')}
{flow_html(a['expected_flow'], 'exp', 'Expected behavior')}
</div></section>
<section><h2>3 · Root cause (source analysis)</h2><div class="prose rc">{codify(html.escape(a['root_cause']))}</div></section>
<section><h2>4 · Suggested response</h2>
<div class="prose resp"><button class="copybtn" onclick="navigator.clipboard.writeText(document.getElementById('resp').innerText).then(()=>this.textContent='✓ copied')">copy</button><span id="resp">{codify(html.escape(a['suggested_response']))}</span></div></section>"""
    else:
        body_sections = '<section><div class="prose">No analysis file for this issue — see it on GitHub above.</div></section>'

    page = f"""<!DOCTYPE html>
<html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width, initial-scale=1">
<title>#{n} · {html.escape(SITE_TITLE)}</title><style>{DETAIL_CSS}</style></head><body>
<a class="back" href="/">← All issues</a>
<div class="hdr">
  <div class="row1"><span class="num">#{n}</span><span class="b {badge}">{label_txt}</span>
    <span class="num" style="margin-left:auto">{age_days(it['created'])}d old · 💬 {it['comments']}</span></div>
  <div class="title">{html.escape(it['title'])}</div>
  <div class="row1"><span class="mod">{html.escape(module_of(it['labels']))}</span>
    <span class="num">by {html.escape(it['author'])}</span></div>
  <a class="ghlink" href="{html.escape(it['url'])}" target="_blank" rel="noopener">Open on GitHub ↗</a>
</div>
{body_sections}
<footer>Generated {now.strftime('%Y-%m-%d %H:%M UTC')} · AI-assisted analysis — verify before posting</footer>
</body></html>"""
    with open(os.path.join(OUT_DIR, f"{n}.html"), "w") as f:
        f.write(page)

# ---------- index ----------
rows = []
for it in sorted(issues, key=lambda x: x["created"], reverse=True):
    n = it["number"]
    _, badge, label_txt = badge_bits(it)
    rows.append(f"""
  <a class="card" href="/{n}">
    <div class="row1"><span class="num">#{n}</span><span class="b {badge}">{label_txt}</span>
      <span class="meta">{age_days(it['created'])}d old · 💬 {it['comments']}</span></div>
    <div class="title">{html.escape(it['title'])}</div>
    <div class="row2"><span class="mod">{html.escape(module_of(it['labels']))}</span>
      <span class="author">by {html.escape(it['author'])}</span></div>
  </a>""")

n_total = len(issues)
bugs = sum(1 for i in issues if issue_type(i["labels"]) == "bug")
feats = sum(1 for i in issues if issue_type(i["labels"]) == "feature")
oldest = max(age_days(i["created"]) for i in issues)

index = f"""<!DOCTYPE html>
<html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width, initial-scale=1">
<title>{html.escape(SITE_TITLE)}</title><style>{INDEX_CSS}</style></head><body>
<h1><span class="repo">{html.escape(REPO)}</span> · open issues</h1>
<div class="sub">Open issues · generated {now.strftime('%Y-%m-%d %H:%M UTC')}</div>
<div class="stats">
  <div class="stat"><div class="v">{n_total}</div><div class="k">total</div></div>
  <div class="stat"><div class="v red">{bugs}</div><div class="k">bugs</div></div>
  <div class="stat"><div class="v green">{feats}</div><div class="k">features</div></div>
  <div class="stat"><div class="v blue">{oldest}d</div><div class="k">oldest</div></div>
</div>
{''.join(rows)}
<footer>Tap a card for full analysis · <a href="https://github.com/{REPO}/issues">live query</a></footer>
</body></html>"""

with open(os.path.join(OUT_DIR, "index.html"), "w") as f:
    f.write(index)

print(f"wrote index + {n_total} detail pages to {OUT_DIR} ({len(analyses)} analyses attached)")
