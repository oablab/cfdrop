//! Local cache of the provisioned temporary account so repeated deploys reuse
//! the same account while its credentials and claim URL remain valid
//! (mirrors wrangler's behavior).

use anyhow::{Context, Result};
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TempAccount {
    pub account_id: String,
    pub account_name: String,
    pub api_token: String,
    pub account_expires_at: DateTime<Utc>,
    pub claim_url: String,
    pub claim_expires_at: DateTime<Utc>,
}

impl TempAccount {
    /// Usable if both the account credentials and the claim window are still
    /// valid, with a safety margin so we do not deploy into an account that
    /// expires mid-upload.
    pub fn is_usable(&self, now: DateTime<Utc>, margin: Duration) -> bool {
        self.account_expires_at > now + margin && self.claim_expires_at > now + margin
    }
}

pub fn state_path() -> Result<PathBuf> {
    let dir = dirs::config_dir()
        .context("could not determine the user config directory")?
        .join("cftmp");
    Ok(dir.join("state.json"))
}

pub fn load(path: &PathBuf) -> Option<TempAccount> {
    let raw = fs::read_to_string(path).ok()?;
    serde_json::from_str(&raw).ok()
}

pub fn save(path: &PathBuf, account: &TempAccount) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    let raw = serde_json::to_string_pretty(account)?;
    fs::write(path, raw).with_context(|| format!("writing {}", path.display()))?;
    // The state file contains a bearer API token and claim URL: restrict perms.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(path, fs::Permissions::from_mode(0o600));
    }
    Ok(())
}

pub fn clear(path: &PathBuf) {
    let _ = fs::remove_file(path);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn account(acct_mins: i64, claim_mins: i64) -> TempAccount {
        let now = Utc::now();
        TempAccount {
            account_id: "acc".into(),
            account_name: "name".into(),
            api_token: "tok".into(),
            account_expires_at: now + Duration::minutes(acct_mins),
            claim_url: "https://dash.cloudflare.com/claim-preview?claimToken=x".into(),
            claim_expires_at: now + Duration::minutes(claim_mins),
        }
    }

    #[test]
    fn usable_when_both_windows_open() {
        let a = account(50, 50);
        assert!(a.is_usable(Utc::now(), Duration::minutes(5)));
    }

    #[test]
    fn unusable_when_claim_window_nearly_closed() {
        let a = account(50, 3);
        assert!(!a.is_usable(Utc::now(), Duration::minutes(5)));
    }

    #[test]
    fn unusable_when_account_expired() {
        let a = account(-1, 50);
        assert!(!a.is_usable(Utc::now(), Duration::minutes(5)));
    }

    #[test]
    fn roundtrips_through_disk() {
        let dir = std::env::temp_dir().join(format!("cftmp-test-{}", std::process::id()));
        let path = dir.join("state.json");
        let a = account(50, 50);
        save(&path, &a).unwrap();
        let loaded = load(&path).unwrap();
        assert_eq!(loaded.account_id, a.account_id);
        assert_eq!(loaded.api_token, a.api_token);
        clear(&path);
        assert!(load(&path).is_none());
        let _ = std::fs::remove_dir_all(dir);
    }
}
