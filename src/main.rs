mod cf;
mod manifest;
mod md;
mod pow;
mod state;

use anyhow::{bail, Context, Result};
use base64::Engine;
use chrono::{Duration, Utc};
use clap::{Parser, Subcommand};
use std::io::Write;
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "cfdrop", version, about = "Deploy a directory to a temporary Cloudflare account and get a live workers.dev URL")]
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
        /// Protect the site with HTTP Basic Auth, format "user:pass"
        #[arg(long, value_name = "USER:PASS")]
        auth: Option<String>,
        /// Treat the directory as Markdown: convert *.md to mobile-friendly
        /// HTML pages (auto-generated index unless index.md exists)
        #[arg(long)]
        md: bool,
    },
    /// Show the cached temporary account (claim URL, expiry)
    Status,
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
        } => deploy(directory, name, yes, fresh, auth, md),
        Command::Status => status(),
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

fn deploy(
    directory: PathBuf,
    name: Option<String>,
    yes: bool,
    fresh: bool,
    auth: Option<String>,
    md: bool,
) -> Result<()> {
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

    // 2. Get a temporary account (cached or fresh)
    let client = cf::CfClient::new()?;
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

    // 3. Upload assets
    let session = client.start_upload_session(&account, &script_name, &entries)?;
    let completion_jwt = client.upload_assets(&account, &session, &entries)?;

    // 4. Deploy the Worker (with optional Basic Auth guard) and enable workers.dev
    client.deploy_worker(&account, &script_name, &completion_jwt, auth_token.as_deref())?;
    client.enable_workers_dev(&account, &script_name)?;
    let subdomain = client.get_subdomain(&account)?;

    let url = format!("https://{script_name}.{subdomain}.workers.dev");
    let minutes_left = (account.claim_expires_at - Utc::now()).num_minutes().max(0);

    println!();
    println!("✅ Deployed: {url}");
    if auth_token.is_some() {
        println!("   Protected with HTTP Basic Auth (--auth).");
    }
    println!();
    println!("This temporary account expires in ~{minutes_left} minutes.");
    println!("Keep it by claiming: {}", account.claim_url);
    if let Some(staged) = staging {
        let _ = std::fs::remove_dir_all(staged);
    }
    Ok(())
}

fn status() -> Result<()> {
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
