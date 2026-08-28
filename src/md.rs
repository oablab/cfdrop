//! `--md` mode: convert a directory of Markdown files into mobile-friendly
//! HTML pages (dark theme, vertical-scroll layout) in a staging directory,
//! which is then deployed like any other asset directory.
//!
//! - `*.md` → `*.html`, wrapped in the mobile template
//! - everything else is copied through unchanged (images, css, ...)
//! - if the source has no `index.md`/`index.html`, an index page listing all
//!   converted pages as tappable cards is generated

use anyhow::{bail, Context, Result};
use pulldown_cmark::{html, CodeBlockKind, Event, Options, Parser, Tag, TagEnd};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use syntect::highlighting::ThemeSet;
use syntect::html::highlighted_html_for_string;
use syntect::parsing::SyntaxSet;
use walkdir::WalkDir;

const CSS: &str = r#"
:root { --bg:#0f1117; --card:#1a1d26; --fg:#e6e8ee; --dim:#8b90a0; --line:#2a2e3b;
        --orange:#ff9900; --red:#f2555a; --green:#4cc38a; --blue:#6ca0f5; }
* { box-sizing:border-box; margin:0; min-width:0; }
html, body { overflow-x:hidden; }
body { background:var(--bg); color:var(--fg); font:16px/1.6 -apple-system,BlinkMacSystemFont,"Segoe UI",Roboto,sans-serif;
       padding:16px; max-width:720px; margin:0 auto; -webkit-text-size-adjust:100%; }
a { color:var(--blue); }
.back { display:inline-block; margin-bottom:12px; font-size:.85rem; text-decoration:none; }
.md { overflow-wrap:anywhere; }
.md h1 { font-size:1.3rem; margin:20px 0 10px; color:var(--orange); }
.md h2 { font-size:1.1rem; margin:18px 0 8px; border-bottom:1px solid var(--line); padding-bottom:5px; }
.md h3 { font-size:.98rem; margin:14px 0 6px; }
.md h1:first-child { margin-top:0; }
.md p, .md ul, .md ol { margin-bottom:12px; font-size:.94rem; }
.md ul, .md ol { padding-left:22px; }
.md li { margin-bottom:4px; }
.md code { background:#232734; border:1px solid var(--line); border-radius:5px; padding:0 4px;
           font:.85em ui-monospace,SFMono-Regular,Menlo,monospace; color:var(--blue); overflow-wrap:anywhere; }
.md pre { background:#0b0d12; border:1px solid var(--line); border-radius:10px; padding:12px;
          overflow-x:auto; margin-bottom:12px;
          font:.8rem/1.5 ui-monospace,SFMono-Regular,Menlo,monospace; }
.md pre code { background:none; border:0; padding:0; color:var(--fg); font-size:1em; }
.md pre.mermaid { background:var(--card); text-align:center; font:inherit; }
.md pre.mermaid svg { max-width:100%; height:auto; }
.md blockquote { border-left:3px solid var(--orange); padding:4px 12px; color:var(--dim);
                 margin-bottom:12px; background:var(--card); border-radius:0 8px 8px 0; }
.md .tablewrap { overflow-x:auto; margin-bottom:12px; border:1px solid var(--line); border-radius:10px; }
.md table { border-collapse:collapse; font-size:.85rem; min-width:100%; }
.md th, .md td { border:1px solid var(--line); padding:6px 10px; text-align:left; }
.md th { background:var(--card); }
.md img { max-width:100%; height:auto; border-radius:10px; }
.md hr { border:0; border-top:1px solid var(--line); margin:18px 0; }
.card { display:block; width:100%; background:var(--card); border:1px solid var(--line); border-radius:12px;
        padding:14px; margin-bottom:10px; text-decoration:none; color:var(--fg); font-weight:600;
        overflow-wrap:anywhere; }
.card:active { background:#232734; }
.card .path { display:block; font-weight:400; font-size:.78rem; color:var(--dim); margin-top:3px;
              font-family:ui-monospace,SFMono-Regular,Menlo,monospace; }
footer { color:var(--dim); font-size:.75rem; text-align:center; margin:24px 0 8px; }
"#;

fn escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// Syntax-highlight `code` for the fenced-block `lang` token. Returns styled
/// `<pre>...</pre>` HTML (inline color spans, baked at build time — no JS),
/// or None when the language is unknown.
fn highlight(code: &str, lang: &str) -> Option<String> {
    if lang.is_empty() {
        return None;
    }
    static SS: OnceLock<SyntaxSet> = OnceLock::new();
    static TS: OnceLock<ThemeSet> = OnceLock::new();
    let ss = SS.get_or_init(SyntaxSet::load_defaults_newlines);
    let ts = TS.get_or_init(ThemeSet::load_defaults);
    let syntax = ss.find_syntax_by_token(lang).or_else(|| {
        // Tokens common in fences but absent from syntect's default grammars:
        // approximate with a close relative rather than falling back to plain.
        let alias = match lang {
            "ts" | "tsx" | "typescript" => "js",
            "jsonc" | "json5" => "json",
            "shell" | "zsh" => "bash",
            "vue" | "svelte" => "html",
            _ => return None,
        };
        ss.find_syntax_by_token(alias)
    })?;
    let theme = &ts.themes["base16-ocean.dark"];
    let rendered = highlighted_html_for_string(code, ss, syntax, theme).ok()?;
    // Drop syntect's inline background on <pre> so the page CSS controls it.
    let rest = rendered.strip_prefix("<pre")?;
    let after_attrs = rest.find('>')?;
    Some(format!("<pre>{}", &rest[after_attrs + 1..]))
}

/// Render markdown to an HTML fragment (tables, strikethrough, footnotes,
/// task lists enabled). Fenced code blocks with a language tag are
/// syntax-highlighted; tables get wrapped so wide ones scroll inside the
/// block instead of the page.
pub fn md_to_html(md: &str) -> String {
    let mut opts = Options::empty();
    opts.insert(Options::ENABLE_TABLES);
    opts.insert(Options::ENABLE_STRIKETHROUGH);
    opts.insert(Options::ENABLE_FOOTNOTES);
    opts.insert(Options::ENABLE_TASKLISTS);

    let mut events: Vec<Event> = Vec::new();
    // (lang, accumulated code) while inside a fenced/indented code block
    let mut code_block: Option<(String, String)> = None;

    for ev in Parser::new_ext(md, opts) {
        match ev {
            Event::Start(Tag::CodeBlock(kind)) => {
                let lang = match &kind {
                    CodeBlockKind::Fenced(l) => l
                        .split(|c: char| c == ',' || c.is_whitespace())
                        .next()
                        .unwrap_or("")
                        .to_string(),
                    CodeBlockKind::Indented => String::new(),
                };
                code_block = Some((lang, String::new()));
            }
            Event::Text(t) if code_block.is_some() => {
                code_block.as_mut().unwrap().1.push_str(&t);
            }
            Event::End(TagEnd::CodeBlock) if code_block.is_some() => {
                let (lang, code) = code_block.take().unwrap();
                let block = if lang == "mermaid" {
                    // Rendered client-side by the bundled mermaid.js; textContent
                    // is decoded by the browser, so escaping is safe here.
                    format!(r#"<pre class="mermaid">{}</pre>"#, escape(&code))
                } else {
                    highlight(&code, &lang).unwrap_or_else(|| {
                        format!("<pre><code>{}</code></pre>", escape(&code))
                    })
                };
                events.push(Event::Html(block.into()));
            }
            other => events.push(other),
        }
    }

    let mut out = String::new();
    html::push_html(&mut out, events.into_iter());
    out.replace("<table>", r#"<div class="tablewrap"><table>"#)
        .replace("</table>", "</table></div>")
}

/// First `# ` heading, or the fallback.
pub fn title_of(md: &str, fallback: &str) -> String {
    md.lines()
        .find_map(|l| l.trim().strip_prefix("# ").map(|t| t.trim().to_string()))
        .filter(|t| !t.is_empty())
        .unwrap_or_else(|| fallback.to_string())
}

const MERMAID_JS: &str = include_str!("vendor/mermaid.min.js");
const MERMAID_MARKER: &str = r#"<pre class="mermaid">"#;

fn render_page(title: &str, body: &str, with_back: bool) -> String {
    let back = if with_back {
        r#"<a class="back" href="/">← Index</a>"#
    } else {
        ""
    };
    let mermaid = if body.contains(MERMAID_MARKER) {
        r#"<script src="/_cftmp/mermaid.min.js"></script><script>mermaid.initialize({startOnLoad:true,theme:"dark"});</script>"#
    } else {
        ""
    };
    format!(
        "<!DOCTYPE html>\n<html lang=\"en\"><head><meta charset=\"utf-8\">\
<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\
<title>{}</title><style>{CSS}</style></head><body>{back}\
<div class=\"md\">{body}</div>\
<footer>deployed with cftmp</footer>{mermaid}</body></html>",
        escape(title)
    )
}

/// Convert `src` into a staging directory of deployable HTML. Returns the
/// staging path; caller deploys it and removes it afterwards.
pub fn stage_directory(src: &Path) -> Result<PathBuf> {
    if !src.is_dir() {
        bail!("{} is not a directory", src.display());
    }
    static STAGE_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let seq = STAGE_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let dest = std::env::temp_dir().join(format!(
        "cftmp-md-stage-{}-{seq}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&dest);
    fs::create_dir_all(&dest).context("creating staging directory")?;

    // (href, title) of every page, for the auto-index
    let mut pages: Vec<(String, String)> = Vec::new();
    let mut have_index = false;
    let mut needs_mermaid = false;

    for item in WalkDir::new(src).follow_links(false) {
        let item = item.context("walking source directory")?;
        if !item.file_type().is_file() {
            continue;
        }
        let rel = item.path().strip_prefix(src).context("relative path")?;
        if rel
            .components()
            .any(|c| c.as_os_str().to_string_lossy().starts_with('.'))
        {
            continue;
        }

        let is_md = item
            .path()
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("md"));

        let out_rel = if is_md {
            rel.with_extension("html")
        } else {
            rel.to_path_buf()
        };
        let out_path = dest.join(&out_rel);
        if let Some(parent) = out_path.parent() {
            fs::create_dir_all(parent)?;
        }

        if is_md {
            let raw = fs::read_to_string(item.path())
                .with_context(|| format!("reading {}", item.path().display()))?;
            let stem = rel.file_stem().unwrap_or_default().to_string_lossy().to_string();
            let title = title_of(&raw, &stem);
            let rel_str = out_rel.to_string_lossy().replace('\\', "/");
            let is_index = rel_str == "index.html";
            let body_html = md_to_html(&raw);
            if body_html.contains(MERMAID_MARKER) {
                needs_mermaid = true;
            }
            let page = render_page(&title, &body_html, !is_index);
            fs::write(&out_path, page)?;
            if is_index {
                have_index = true;
            } else {
                // extensionless link (html_handling serves it)
                let href = format!("/{}", rel_str.trim_end_matches(".html"));
                pages.push((href, title));
            }
        } else {
            if out_rel.to_string_lossy() == "index.html" {
                have_index = true;
            }
            fs::copy(item.path(), &out_path)
                .with_context(|| format!("copying {}", item.path().display()))?;
        }
    }

    if needs_mermaid {
        let vendor_dir = dest.join("_cftmp");
        fs::create_dir_all(&vendor_dir)?;
        fs::write(vendor_dir.join("mermaid.min.js"), MERMAID_JS)
            .context("writing bundled mermaid.min.js")?;
    }

    if pages.is_empty() && !have_index {
        bail!("no markdown files found in {}", src.display());
    }

    if !have_index {
        pages.sort();
        let dir_name = src
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "pages".into());
        let cards: String = pages
            .iter()
            .map(|(href, title)| {
                format!(
                    r#"<a class="card" href="{}">{}<span class="path">{}</span></a>"#,
                    escape(href),
                    escape(title),
                    escape(href)
                )
            })
            .collect();
        let body = format!("<h1>{}</h1>{cards}", escape(&dir_name));
        fs::write(dest.join("index.html"), render_page(&dir_name, &body, false))?;
    }

    Ok(dest)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_code_and_tables() {
        let out = md_to_html("run `hashFile()` now\n\n| a | b |\n|---|---|\n| 1 | 2 |\n");
        assert!(out.contains("<code>hashFile()</code>"));
        assert!(out.contains(r#"<div class="tablewrap"><table>"#));
        assert!(out.contains("</table></div>"));
    }

    #[test]
    fn highlights_known_language() {
        let out = md_to_html("```rust\nlet x: u32 = 1;\n```\n");
        assert!(out.contains("<span style=\"color:"), "expected colored spans, got: {out}");
        assert!(!out.contains("<pre style="), "pre background must be stripped");
    }

    #[test]
    fn typescript_fence_highlights_via_js_alias() {
        let out = md_to_html("```ts\nconst x = { a: 1 };\n```\n");
        assert!(out.contains("<span style=\"color:"), "ts should highlight via js alias: {out}");
    }

    #[test]
    fn unknown_language_falls_back_to_plain() {
        let out = md_to_html("```notalang\nfoo & <bar>\n```\n");
        assert!(out.contains("<pre><code>foo &amp; &lt;bar&gt;\n</code></pre>"));
        assert!(!out.contains("<span style=\"color:"));
    }

    #[test]
    fn plain_fence_is_escaped_not_highlighted() {
        let out = md_to_html("```\na < b\n```\n");
        assert!(out.contains("a &lt; b"));
        assert!(!out.contains("<span style=\"color:"));
    }

    #[test]
    fn mermaid_fence_becomes_mermaid_pre() {
        let out = md_to_html("```mermaid\ngraph TD\n  A --> B\n```\n");
        assert!(out.contains(r#"<pre class="mermaid">graph TD"#));
        assert!(out.contains("A --&gt; B")); // escaped; browser decodes textContent
        assert!(!out.contains("<span style=\"color:"));
    }

    #[test]
    fn mermaid_asset_and_script_only_when_used() {
        let src = std::env::temp_dir().join(format!("cftmp-md-src-mm-{}", std::process::id()));
        let _ = fs::remove_dir_all(&src);
        fs::create_dir_all(&src).unwrap();
        fs::write(src.join("diagram.md"), "# D\n\n```mermaid\ngraph TD\nA-->B\n```\n").unwrap();
        fs::write(src.join("plain.md"), "# P\n\ntext only").unwrap();

        let dest = stage_directory(&src).unwrap();
        assert!(dest.join("_cftmp/mermaid.min.js").is_file());
        let diagram = fs::read_to_string(dest.join("diagram.html")).unwrap();
        assert!(diagram.contains(r#"src="/_cftmp/mermaid.min.js""#));
        assert!(diagram.contains("mermaid.initialize"));
        let plain = fs::read_to_string(dest.join("plain.html")).unwrap();
        assert!(!plain.contains("mermaid.min.js"));

        let _ = fs::remove_dir_all(&src);
        let _ = fs::remove_dir_all(&dest);
    }

    #[test]
    fn no_mermaid_asset_when_unused() {
        let src = std::env::temp_dir().join(format!("cftmp-md-src-nomm-{}", std::process::id()));
        let _ = fs::remove_dir_all(&src);
        fs::create_dir_all(&src).unwrap();
        fs::write(src.join("a.md"), "# A\n\nplain").unwrap();
        let dest = stage_directory(&src).unwrap();
        assert!(!dest.join("_cftmp").exists());
        let _ = fs::remove_dir_all(&src);
        let _ = fs::remove_dir_all(&dest);
    }

    #[test]
    fn extracts_title() {
        assert_eq!(title_of("# Hello World\nbody", "fb"), "Hello World");
        assert_eq!(title_of("no heading here", "fb"), "fb");
    }

    #[test]
    fn stages_md_dir_with_auto_index() {
        let src = std::env::temp_dir().join(format!("cftmp-md-src-{}", std::process::id()));
        let _ = fs::remove_dir_all(&src);
        fs::create_dir_all(src.join("sub")).unwrap();
        fs::write(src.join("alpha.md"), "# Alpha Page\n\nhello `code`").unwrap();
        fs::write(src.join("sub/beta.md"), "no heading").unwrap();
        fs::write(src.join("logo.txt"), "asset").unwrap();

        let dest = stage_directory(&src).unwrap();
        assert!(dest.join("alpha.html").is_file());
        assert!(dest.join("sub/beta.html").is_file());
        assert!(dest.join("logo.txt").is_file());
        let index = fs::read_to_string(dest.join("index.html")).unwrap();
        assert!(index.contains(r#"href="/alpha""#));
        assert!(index.contains("Alpha Page"));
        assert!(index.contains(r#"href="/sub/beta""#));
        let alpha = fs::read_to_string(dest.join("alpha.html")).unwrap();
        assert!(alpha.contains("<code>code</code>"));
        assert!(alpha.contains("← Index"));
        assert!(alpha.contains("overflow-x:hidden"));

        let _ = fs::remove_dir_all(&src);
        let _ = fs::remove_dir_all(&dest);
    }

    #[test]
    fn respects_existing_index_md() {
        let src = std::env::temp_dir().join(format!("cftmp-md-src-idx-{}", std::process::id()));
        let _ = fs::remove_dir_all(&src);
        fs::create_dir_all(&src).unwrap();
        fs::write(src.join("index.md"), "# My Home\n\ncustom").unwrap();
        fs::write(src.join("other.md"), "# Other").unwrap();

        let dest = stage_directory(&src).unwrap();
        let index = fs::read_to_string(dest.join("index.html")).unwrap();
        assert!(index.contains("My Home"));
        assert!(!index.contains(r#"class="card""#)); // no auto-index
        assert!(!index.contains("← Index")); // index has no back link

        let _ = fs::remove_dir_all(&src);
        let _ = fs::remove_dir_all(&dest);
    }

    #[test]
    fn rejects_dir_without_markdown() {
        let src = std::env::temp_dir().join(format!("cftmp-md-src-none-{}", std::process::id()));
        let _ = fs::remove_dir_all(&src);
        fs::create_dir_all(&src).unwrap();
        fs::write(src.join("data.txt"), "x").unwrap();
        assert!(stage_directory(&src).is_err());
        let _ = fs::remove_dir_all(&src);
    }
}
