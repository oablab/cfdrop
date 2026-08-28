mod cf;
mod manifest;
mod pow;
mod state;

use anyhow::{bail, Context, Result};
use chrono::{Duration, Utc};
use clap::{Parser, Subcommand};
use std::io::Write;
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "cftmp", version, about = "Deploy a directory to a temporary Cloudflare account and get a live workers.dev URL")]
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
        } => deploy(directory, name, yes, fresh),
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
        "cftmp-site".to_string()
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

fn deploy(directory: PathBuf, name: Option<String>, yes: bool, fresh: bool) -> Result<()> {
    let directory = directory
        .canonicalize()
        .with_context(|| format!("directory not found: {}", directory.display()))?;

    let script_name = sanitize_name(
        &name.unwrap_or_else(|| {
            directory
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| "cftmp-site".into())
        }),
    );

    // 1. Build the asset manifest
    let entries = manifest::build_manifest(&directory)?;
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

    // 4. Deploy the assets-only Worker and enable workers.dev
    client.deploy_worker(&account, &script_name, &completion_jwt)?;
    client.enable_workers_dev(&account, &script_name)?;
    let subdomain = client.get_subdomain(&account)?;

    let url = format!("https://{script_name}.{subdomain}.workers.dev");
    let minutes_left = (account.claim_expires_at - Utc::now()).num_minutes().max(0);

    println!();
    println!("✅ Deployed: {url}");
    println!();
    println!("This temporary account expires in ~{minutes_left} minutes.");
    println!("Keep it by claiming: {}", account.claim_url);
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
        None => println!("No cached temporary account. Run `cftmp deploy` to create one."),
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
        assert_eq!(sanitize_name("---"), "cftmp-site");
        assert_eq!(sanitize_name("Already-ok-123"), "already-ok-123");
    }
}
