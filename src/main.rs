mod cf;
mod manifest;
mod md;
mod pow;
mod state;
mod target;

use anyhow::{bail, Context, Result};
use base64::Engine;
use chrono::{Duration, Utc};
use state::Credentials;
use target::Mode;
use clap::{Parser, Subcommand};
use std::io::Write;
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "cfdrop", version, about = "Deploy a directory to Cloudflare Workers — a temporary account by default, or your own account with --own")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Bundle a directory and deploy it to a temporary Cloudflare account
    Deploy {
        /// Directory to deploy
        #[arg(short, long)]
        directory: PathBuf,
        /// Worker (site) name; defaults to the directory name
        #[arg(short, long)]
        name: Option<String>,
        /// Accept Cloudflare's Terms of Service and Privacy Policy without prompting
        #[arg(short = 'y', long)]
        yes: bool,
        /// Force provisioning a fresh temporary account even if a cached one is still valid
        #[arg(long)]
        fresh: bool,
        /// Deploy into YOUR Cloudflare account instead of a temporary one.
        /// Token from CLOUDFLARE_API_TOKEN (or --token-file); account from
        /// --account / CLOUDFLARE_ACCOUNT_ID, or inferred when the token sees
        /// exactly one account. No 60-minute expiry, no claim step.
        #[arg(long)]
        own: bool,
        /// Cloudflare account id to deploy into (implies --own)
        #[arg(long, value_name = "ACCOUNT_ID")]
        account: Option<String>,
        /// Read the API token from this file instead of CLOUDFLARE_API_TOKEN
        /// (never pass the token itself on the command line)
        #[arg(long, value_name = "PATH")]
        token_file: Option<String>,
        /// Use a temporary account even if CLOUDFLARE_ACCOUNT_ID is set
        #[arg(long, conflicts_with_all = ["own", "account"])]
        temporary: bool,
        /// Own-account mode: overwrite a Worker of the same name even if
        /// cfdrop did not create it
        #[arg(long)]
        force: bool,
        /// Protect the site with HTTP Basic Auth, format "user:pass"
        #[arg(long, value_name = "USER:PASS")]
        auth: Option<String>,
        /// Treat the directory as Markdown: convert *.md to mobile-friendly
        /// HTML pages (auto-generated index unless index.md exists)
        #[arg(long)]
        md: bool,
        /// After a successful deploy, push the URL to a cfdrop relay so a
        /// paired viewer opens it. Endpoint from CFDROP_NOTIFY, token from
        /// CFDROP_RELAY_TOKEN.
        #[arg(long)]
        notify: bool,
    },
    /// Show the cached temporary account (claim URL, expiry)
    Status,
    /// Delete a site from YOUR account (own-account mode only). Temporary
    /// sites expire on their own.
    Rm {
        /// Worker (site) name
        #[arg(short, long)]
        name: String,
        /// Cloudflare account id (or CLOUDFLARE_ACCOUNT_ID; inferred if the token sees one account)
        #[arg(long, value_name = "ACCOUNT_ID")]
        account: Option<String>,
        /// Read the API token from this file instead of CLOUDFLARE_API_TOKEN
        #[arg(long, value_name = "PATH")]
        token_file: Option<String>,
        /// Delete even if cfdrop did not create the Worker
        #[arg(long)]
        force: bool,
    },
    /// Forget the cached temporary account
    Logout,
}

fn main() {
    if let Err(err) = run() {
        eprintln!("error: {err:#}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::Deploy {
            directory,
            name,
            yes,
            fresh,
            auth,
            md,
            notify,
            own,
            account,
            token_file,
            temporary,
            force,
        } => {
            let inputs = target::Inputs::from_env(own, temporary, account, token_file);
            let mode = target::resolve(&inputs, |p| Ok(std::fs::read_to_string(p)?))?;
            deploy(DeployOpts { directory, name, yes, fresh, auth, md, notify, mode, force })
        }
        Command::Status => status(),
        Command::Rm { name, account, token_file, force } => {
            let inputs = target::Inputs::from_env(true, false, account, token_file);
            let mode = target::resolve(&inputs, |p| Ok(std::fs::read_to_string(p)?))?;
            rm(name, mode, force)
        }
        Command::Logout => {
            let path = state::state_path()?;
            state::clear(&path);
            println!("Cached temporary account removed.");
            Ok(())
        }
    }
}

fn sanitize_name(raw: &str) -> String {
    let mut s: String = raw
        .to_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    // Collapse consecutive dashes and trim
    while s.contains("--") {
        s = s.replace("--", "-");
    }
    let s = s.trim_matches('-').to_string();
    if s.is_empty() {
        "cfdrop-site".to_string()
    } else {
        s.chars().take(54).collect()
    }
}

fn confirm_terms() -> Result<bool> {
    eprintln!(
        "Continuing creates a temporary Cloudflare account and means you accept:\n  Terms of Service: {}\n  Privacy Policy:   {}",
        cf::TERMS_URL,
        cf::PRIVACY_URL
    );
    eprint!("Proceed? [y/N] ");
    std::io::stderr().flush().ok();
    let mut line = String::new();
    std::io::stdin().read_line(&mut line)?;
    Ok(matches!(line.trim().to_lowercase().as_str(), "y" | "yes"))
}

struct DeployOpts {
    directory: PathBuf,
    name: Option<String>,
    yes: bool,
    fresh: bool,
    auth: Option<String>,
    md: bool,
    notify: bool,
    mode: Mode,
    force: bool,
}

fn deploy(opts: DeployOpts) -> Result<()> {
    let DeployOpts { directory, name, yes, fresh, auth, md, notify, mode, force } = opts;
    if fresh && matches!(mode, Mode::Own { .. }) {
        bail!("--fresh only applies to temporary accounts; drop it when using --own/--account");
    }
    // Validate and encode the Basic Auth credential up front
    let auth_token = match &auth {
        Some(cred) => {
            let (user, pass) = cred
                .split_once(':')
                .context("--auth must be in the form user:pass")?;
            if user.is_empty() || pass.is_empty() {
                bail!("--auth must be in the form user:pass (both non-empty)");
            }
            Some(base64::engine::general_purpose::STANDARD.encode(cred))
        }
        None => None,
    };

    let directory = directory
        .canonicalize()
        .with_context(|| format!("directory not found: {}", directory.display()))?;

    let script_name = sanitize_name(
        &name.unwrap_or_else(|| {
            directory
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| "cfdrop-site".into())
        }),
    );

    // Optionally convert Markdown into a staged HTML directory
    let staging = if md {
        let staged = md::stage_directory(&directory)?;
        eprintln!("Converted Markdown to mobile HTML ({} staged).", staged.display());
        Some(staged)
    } else {
        None
    };
    let source_dir = staging.as_deref().unwrap_or(&directory);

    // 1. Build the asset manifest
    let entries = manifest::build_manifest(source_dir)?;
    let total_bytes: u64 = entries.iter().map(|e| e.size).sum();
    eprintln!(
        "Found {} file(s), {:.1} KiB total.",
        entries.len(),
        total_bytes as f64 / 1024.0
    );

    // 2. Resolve who we deploy as
    let client = cf::CfClient::new()?;
    enum Target {
        Temp(state::TempAccount),
        Own { creds: Credentials, account_name: String, token_source: &'static str },
    }
    let target = match mode {
        Mode::Temporary => {
            let state_path = state::state_path()?;
            let margin = Duration::minutes(5);
            let cached = if fresh { None } else { state::load(&state_path) };
            let (account, reused) = match cached {
                Some(acc) if acc.is_usable(Utc::now(), margin) => (acc, true),
                _ => {
                    if !yes && !confirm_terms()? {
                        bail!("aborted: terms not accepted");
                    }
                    eprintln!("Provisioning temporary Cloudflare account...");
                    let acc = client.provision_temp_account()?;
                    state::save(&state_path, &acc)?;
                    (acc, false)
                }
            };
            eprintln!(
                "Temporary account {} ({}), expires {}",
                account.account_name,
                if reused { "reused" } else { "created" },
                account.account_expires_at.format("%H:%M UTC")
            );
            Target::Temp(account)
        }
        Mode::Own { account_id, api_token, token_source } => {
            let (creds, account_name) = resolve_own(&client, account_id.as_deref(), api_token)?;
            eprintln!("Deploying to your account {} ({}).", account_name, creds.account_id);
            // Never clobber a Worker cfdrop did not create.
            let existing = client.script_tags(&creds, &script_name)?;
            if !target::may_overwrite(existing.as_deref(), force, cf::CFDROP_TAG) {
                bail!(
                    "a Worker named `{script_name}` already exists in account {} and was not \
                     deployed by cfdrop (tags: {:?}). Pick another --name, or pass --force to \
                     overwrite it.",
                    creds.account_id,
                    existing.unwrap_or_default()
                );
            }
            if auth_token.is_some() {
                eprintln!(
                    "warning: --auth bakes the credential into the Worker script. For a \
                     long-lived site prefer Cloudflare Access on the workers.dev hostname."
                );
            }
            Target::Own { creds, account_name, token_source }
        }
    };
    let creds = match &target {
        Target::Temp(acc) => acc.creds(),
        Target::Own { creds, .. } => creds.clone(),
    };

    // 3. Upload assets
    let session = client.start_upload_session(&creds, &script_name, &entries)?;
    let completion_jwt = client.upload_assets(&creds, &session, &entries)?;

    // 4. Deploy the Worker (with optional Basic Auth guard) and enable workers.dev
    client.deploy_worker(&creds, &script_name, &completion_jwt, auth_token.as_deref())?;
    client.enable_workers_dev(&creds, &script_name)?;
    let subdomain = client.get_subdomain(&creds)?;

    let url = format!("https://{script_name}.{subdomain}.workers.dev");

    println!();
    println!("✅ Deployed: {url}");
    if auth_token.is_some() {
        println!("   Protected with HTTP Basic Auth (--auth).");
    }
    println!();
    match &target {
        Target::Temp(account) => {
            let minutes_left = (account.claim_expires_at - Utc::now()).num_minutes().max(0);
            println!("This temporary account expires in ~{minutes_left} minutes.");
            println!("Keep it by claiming: {}", account.claim_url);
        }
        Target::Own { creds, account_name, token_source } => {
            println!("Account: {} ({}) — your own account, token from {token_source}.", account_name, creds.account_id);
            println!("Worker:  {script_name} (tagged `{}`; redeploy with the same --name to update, `cfdrop rm --name {script_name}` to delete)", cf::CFDROP_TAG);
        }
    }
    if notify {
        // A fresh workers.dev deployment can take a few seconds to propagate;
        // opening it on the viewer too early shows a blank page. Wait until
        // the site actually serves before pushing.
        match wait_until_live(&url, std::time::Duration::from_secs(30)) {
            Ok(waited) => {
                if waited.as_millis() > 0 {
                    eprintln!("Site live after {:.1}s.", waited.as_secs_f32());
                }
                match notify_relay(&url) {
                    Ok(endpoint) => println!("Pushed URL to relay at {endpoint}."),
                    Err(e) => eprintln!("warning: relay notify failed: {e:#}"),
                }
            }
            Err(e) => eprintln!("warning: site not reachable yet, skipping notify: {e:#}"),
        }
    }
    if let Some(staged) = staging {
        let _ = std::fs::remove_dir_all(staged);
    }
    Ok(())
}

/// Poll `url` (cache-busted) until it returns 200, or give up after `max`.
/// Returns how long we waited.
fn wait_until_live(url: &str, max: std::time::Duration) -> Result<std::time::Duration> {
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()?;
    let start = std::time::Instant::now();
    let mut attempt = 0u32;
    loop {
        attempt += 1;
        let probe = format!("{url}/?cfdrop-warmup={attempt}");
        let ok = client
            .get(&probe)
            .send()
            .map(|r| r.status().is_success())
            .unwrap_or(false);
        if ok {
            // One more hit on the plain URL so the edge the viewer reaches
            // has served the real path at least once.
            let _ = client.get(url).send();
            return Ok(start.elapsed());
        }
        if start.elapsed() >= max {
            anyhow::bail!("still not serving 200 after {}s", max.as_secs());
        }
        std::thread::sleep(std::time::Duration::from_secs(1));
    }
}

/// POST the deployed URL to the cfdrop relay (see oablab/cfdrop-app).
/// Never fatal: the deploy already succeeded.
fn notify_relay(url: &str) -> Result<String> {
    let endpoint = std::env::var("CFDROP_NOTIFY")
        .context("CFDROP_NOTIFY must be set for --notify")?;
    let token = std::env::var("CFDROP_RELAY_TOKEN")
        .context("CFDROP_RELAY_TOKEN must be set for --notify")?;
    let resp = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()?
        .post(format!("{}/push", endpoint.trim_end_matches('/')))
        .header("x-relay-token", token)
        .body(url.to_string())
        .send()
        .context("sending push to relay")?;
    if !resp.status().is_success() {
        anyhow::bail!("relay returned {}", resp.status());
    }
    Ok(endpoint)
}

/// Own-account preflight: prove the token works and pick the account, using
/// only endpoints that accept both user tokens and account-owned tokens.
fn resolve_own(
    client: &cf::CfClient,
    requested: Option<&str>,
    api_token: String,
) -> Result<(Credentials, String)> {
    let visible: Vec<(String, String)> = client
        .list_accounts(&api_token)?
        .into_iter()
        .map(|a| (a.id, a.name))
        .collect();
    let (account_id, account_name) = target::choose_account(&visible, requested)?;
    Ok((Credentials { account_id, api_token }, account_name))
}

fn rm(name: String, mode: Mode, force: bool) -> Result<()> {
    let Mode::Own { account_id, api_token, .. } = mode else {
        bail!("`cfdrop rm` works on your own account only (set CLOUDFLARE_API_TOKEN and --account / CLOUDFLARE_ACCOUNT_ID); temporary sites expire on their own");
    };
    let script_name = sanitize_name(&name);
    let client = cf::CfClient::new()?;
    let (creds, account_name) = resolve_own(&client, account_id.as_deref(), api_token)?;
    let Some(tags) = client.script_tags(&creds, &script_name)? else {
        bail!("no Worker named `{script_name}` in account {} ({account_name})", creds.account_id);
    };
    if !target::may_overwrite(Some(&tags), force, cf::CFDROP_TAG) {
        bail!(
            "`{script_name}` in account {} was not deployed by cfdrop (tags: {tags:?}); pass --force to delete it anyway",
            creds.account_id
        );
    }
    client.delete_script(&creds, &script_name)?;
    println!("🗑  Deleted Worker `{script_name}` from account {} ({account_name}).", creds.account_id);
    Ok(())
}

fn status() -> Result<()> {
    let own_env = match (
        std::env::var("CLOUDFLARE_API_TOKEN").ok().filter(|v| !v.is_empty()),
        std::env::var("CLOUDFLARE_ACCOUNT_ID").ok().filter(|v| !v.is_empty()),
    ) {
        (Some(_), Some(acc)) => format!("token + account {acc} set → `deploy` goes to your own account"),
        (Some(_), None) => "token set, no account → `deploy` is still temporary (add --own or CLOUDFLARE_ACCOUNT_ID)".to_string(),
        (None, Some(acc)) => format!("account {acc} set but no CLOUDFLARE_API_TOKEN → `deploy` will error"),
        (None, None) => "not set → `deploy` uses a temporary account".to_string(),
    };
    println!("Own-account env: {own_env}");
    println!();
    let path = state::state_path()?;
    match state::load(&path) {
        Some(acc) => {
            let usable = acc.is_usable(Utc::now(), Duration::minutes(0));
            println!("Account:   {} ({})", acc.account_name, acc.account_id);
            println!("Expires:   {}", acc.account_expires_at);
            println!("Claim URL: {}", acc.claim_url);
            println!("Claim by:  {}", acc.claim_expires_at);
            println!("Status:    {}", if usable { "usable" } else { "EXPIRED" });
        }
        None => println!("No cached temporary account. Run `cfdrop deploy` to create one."),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::sanitize_name;

    #[test]
    fn sanitizes_names() {
        assert_eq!(sanitize_name("My Site!"), "my-site");
        assert_eq!(sanitize_name("path.to.dir"), "path-to-dir");
        assert_eq!(sanitize_name("---"), "cfdrop-site");
        assert_eq!(sanitize_name("Already-ok-123"), "already-ok-123");
    }
}
