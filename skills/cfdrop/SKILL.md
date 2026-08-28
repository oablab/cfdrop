---
name: cfdrop
description: Explain or present anything as a mobile-optimized static website deployed to a temporary Cloudflare account via the cfdrop CLI. Use when the user says "cfdrop this", "explain this by cfdrop", "cfdrop mobile optimized", "deploy as mobile site", or asks to turn content (a topic, codebase, issue list, analysis, report) into a browsable mobile web page with a live URL.
---

# cfdrop — Explain Anything as a Mobile Site

Generate mobile-first static web assets that explain the requested subject, deploy them
with the `cfdrop` CLI (self-contained Rust binary, no Cloudflare account needed), and
return a live `workers.dev` URL. Sites live ~60 minutes unless claimed.

## Workflow

1. **Understand the subject.** "Explain this" may point at code, issues, a document, a
   conversation topic. Gather what's needed (read files, fetch issues, run analysis).
   For many independent items needing deep analysis, fan out to subagents that each
   write `/tmp/<slug>-analysis/<id>.json`.
2. **Generate the site** into `/tmp/<slug>-site/` (slug = short kebab-case subject name).

   **Markdown path (prefer for prose-heavy subjects):** write `*.md` files into the
   directory and deploy with `--md` — cfdrop (≥0.2.0) converts them to mobile-first
   dark-theme pages itself (tables, code blocks, blockquotes handled; auto index of
   tappable cards unless you provide `index.md`). Use real backticks for identifiers,
   and language-tagged fences (```rust, ```bash, ...) — cfdrop ≥0.3.0 syntax-highlights
   them at build time (no client JS). ```mermaid fences render as diagrams (cfdrop
   ≥0.4.0 bundles mermaid.js into the site — no CDN); prefer `graph TD` (top-down)
   for mobile. This needs no HTML generation at all.

   **HTML path (only for app-like UIs):** stat-tile dashboards, badge-heavy indexes
   with tappable cards, and copy-to-clipboard buttons need templated HTML. (Plain
   diagrams no longer justify HTML — use mermaid on the markdown path.) Inline the
   CSS from `assets/base.css` (this skill dir) into each page's `<style>`. Structure:
   - `index.html` — header, 2×2 stat tiles (grid), then one full-width tappable card
     per item linking to `/<id>`
   - `<id>.html` — one page per item: header card with "← All items" back link and an
     outbound source link, then numbered sections (summary / current-vs-expected flow
     diagram / analysis / suggested action — adapt sections to the subject)
3. **Deploy:** `cfdrop deploy --directory /tmp/<slug>-site --name <slug> -y`
   - Add `--auth user:pass` if the user wants the site gated (HTTP Basic Auth)
   - `-y` is required non-interactively (accepts Cloudflare ToS)
4. **Verify:** curl the index and one detail page, expect 200. An immediate curl can hit
   a stale edge cache — append `?v=N` or retry once before concluding failure.
5. **Report:** live URL, expiry (~60 min), and the claim URL (printed by cfdrop) so the
   user can keep the account. The claim URL is a bearer credential — show it to the
   user, never publish it on the site itself.

## Mobile design rules (non-negotiable)

- **Vertical scrolling only.** `html,body { overflow-x:hidden }`, `* { min-width:0 }`,
  `overflow-wrap:anywhere` on titles/paths/URLs. Never use wide tables — use stacked
  block cards instead.
- **Block/tile composition.** Stats as a CSS grid (2 columns ≤480px), one card per item,
  full width, generous tap targets (whole card is the `<a>`).
- **Diagrams**: on the markdown path use ```mermaid fences — `graph TD` (top-down) for
  flows, `sequenceDiagram` for interactions; keep node labels short so they fit a phone.
  Current-vs-expected = two small `graph TD` blocks under `## Current` / `## Expected`
  headings. On the HTML path, flow diagrams are stacked boxes joined by `↓` arrows
  (two columns via `grid-template-columns:1fr 1fr`, collapsing to `1fr` ≤560px), each
  step under ~12 words.
- Dark theme tokens, badges, and all of the above are already in `assets/base.css` —
  don't rewrite it, inline it.
- **Every `<textarea>` must have a working copy button.** Wrap each one in
  `.textarea-copy`, place a `.copybtn` inside the wrapper, and copy the textarea's
  current `.value` (not `innerText`). This applies to readonly output and editable
  textareas so user edits are copied too. Use unique labels when multiple textareas
  appear on one page, and include this delegated handler once per page:

  ```html
  <div class="textarea-copy">
    <button type="button" class="copybtn" aria-label="Copy textarea contents">Copy</button>
    <textarea>Text to copy</textarea>
  </div>
  <script>
  document.addEventListener('click', async (event) => {
    const button = event.target.closest('.textarea-copy .copybtn');
    if (!button) return;
    const textarea = button.closest('.textarea-copy').querySelector('textarea');
    const label = button.textContent;
    try {
      await navigator.clipboard.writeText(textarea.value);
    } catch {
      textarea.focus();
      textarea.select();
      document.execCommand('copy');
    }
    button.textContent = '✓ Copied';
    setTimeout(() => { button.textContent = label; }, 1500);
  });
  </script>
  ```
- **Code identifiers**: markdown path — just use real backticks (and language-tagged
  fences); cfdrop renders and highlights them. HTML path — wrap identifiers in `<code>`
  tags (see the `codify()` regex pass in `examples/gen-triage-site.py` in this repo);
  styling is in `assets/base.css` (`.prose code`).
- Link detail pages **without** the `.html` extension (`href="/123"`) — cfdrop deploys
  with `html_handling: auto-trailing-slash`, which serves them.

## cfdrop CLI facts

- Binary: `cfdrop` on PATH (release binaries for linux-amd64/arm64 + macos-arm64 on
  this repo's GitHub releases). Feature floor: `--md` needs ≥0.2.0, syntax
  highlighting ≥0.3.0, mermaid ≥0.4.0 — check `cfdrop --version` if a feature seems
  missing.
- `cfdrop status` — cached temp account, expiry, claim URL; `cfdrop logout` — forget it;
  `--fresh` — force a new account
- The temp account is cached in the OS config dir (e.g.
  `~/Library/Application Support/cfdrop/state.json` on macOS) and reused across deploys
  until expiry — redeploys to the same `--name` update the same URL; different
  `--name`s create sibling sites on one account with a shared clock
- Limits: ≤1,000 files, ≤5 MiB/file; hidden files skipped
- `--auth` bakes the credential into the Worker script — preview-grade only; for
  long-lived sites the user should claim the account and use Cloudflare Access
- If the account expired, deploy just provisions a new one (~1s proof-of-work); old
  URLs die with the old account — re-share the new URL

## Content quality bar

- Index answers "what is this and how much of it" at a glance (counts, type badges,
  age); detail pages answer "what, why, what next" in numbered sections
- For issue/bug subjects: summary in plain language, current-vs-expected flow (mermaid
  on the markdown path, box columns on the HTML path), root cause citing real file
  paths, and a suggested response (HTML path: add a copy button via
  `navigator.clipboard.writeText`)
- Mark AI-generated analysis in the footer: "AI-assisted analysis — verify before posting"
