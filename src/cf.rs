//! Minimal Cloudflare API client for the temporary-account deploy flow.

use crate::manifest::AssetEntry;
use crate::pow::{self, Challenge};
use crate::state::TempAccount;
use anyhow::{anyhow, bail, Context, Result};
use chrono::{DateTime, Utc};
use reqwest::blocking::{multipart, Client};
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::time::Duration;

const API_BASE: &str = "https://api.cloudflare.com/client/v4";
pub const TERMS_URL: &str = "https://www.cloudflare.com/terms/";
pub const PRIVACY_URL: &str = "https://www.cloudflare.com/privacypolicy/";

pub struct CfClient {
    http: Client,
}

#[derive(Deserialize)]
struct Envelope<T> {
    success: bool,
    result: Option<T>,
    #[serde(default)]
    errors: Option<Vec<Value>>,
}

fn unwrap_envelope<T>(env: Envelope<T>, what: &str) -> Result<T> {
    if !env.success {
        bail!(
            "{what} failed: {}",
            serde_json::to_string(&env.errors.unwrap_or_default()).unwrap_or_default()
        );
    }
    env.result.ok_or_else(|| anyhow!("{what}: empty result"))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ChallengeResult {
    challenge_token: String,
    seed: String,
    k: u64,
    g: u64,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PreviewAccount {
    id: String,
    name: String,
    api_token: String,
    expires_at: DateTime<Utc>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PreviewClaim {
    url: String,
    expires_at: DateTime<Utc>,
}

#[derive(Deserialize)]
struct PreviewResult {
    account: PreviewAccount,
    claim: PreviewClaim,
}

#[derive(Deserialize)]
pub struct UploadSession {
    pub jwt: String,
    #[serde(default)]
    buckets: Option<Vec<Vec<String>>>,
}

impl UploadSession {
    pub fn buckets(&self) -> &[Vec<String>] {
        self.buckets.as_deref().unwrap_or(&[])
    }
}

impl CfClient {
    pub fn new() -> Result<Self> {
        let http = Client::builder()
            .timeout(Duration::from_secs(120))
            .user_agent(concat!("cftmp/", env!("CARGO_PKG_VERSION")))
            .build()
            .context("building HTTP client")?;
        Ok(Self { http })
    }

    /// Provision a fresh temporary account: request challenge, solve PoW,
    /// create the account. Caller must have obtained ToS acceptance.
    pub fn provision_temp_account(&self) -> Result<TempAccount> {
        let resp: Envelope<ChallengeResult> = self
            .http
            .post(format!("{API_BASE}/provisioning/previews/challenge"))
            .json(&json!({}))
            .send()
            .context("requesting proof-of-work challenge")?
            .json()
            .context("parsing challenge response")?;
        let ch = unwrap_envelope(resp, "challenge request")?;

        eprintln!(
            "Solving proof-of-work ({} SHA-256 hashes)...",
            ch.k * ch.g
        );
        let checkpoints = pow::solve(&Challenge {
            seed: ch.seed,
            k: ch.k,
            g: ch.g,
        })?;

        let resp: Envelope<PreviewResult> = self
            .http
            .post(format!("{API_BASE}/provisioning/previews"))
            .json(&json!({
                "termsOfService": TERMS_URL,
                "privacyPolicy": PRIVACY_URL,
                "acceptTermsOfService": "yes",
                "challengeToken": ch.challenge_token,
                "solution": { "checkpoints": checkpoints },
            }))
            .send()
            .context("creating temporary account")?
            .json()
            .context("parsing temporary account response")?;
        let r = unwrap_envelope(resp, "temporary account creation")?;

        Ok(TempAccount {
            account_id: r.account.id,
            account_name: r.account.name,
            api_token: r.account.api_token,
            account_expires_at: r.account.expires_at,
            claim_url: r.claim.url,
            claim_expires_at: r.claim.expires_at,
        })
    }

    /// Start an assets upload session; returns the upload JWT and buckets of
    /// file hashes that still need uploading.
    pub fn start_upload_session(
        &self,
        account: &TempAccount,
        script_name: &str,
        entries: &[AssetEntry],
    ) -> Result<UploadSession> {
        let mut manifest = serde_json::Map::new();
        for e in entries {
            manifest.insert(
                e.manifest_path.clone(),
                json!({ "hash": e.hash, "size": e.size }),
            );
        }

        let resp: Envelope<UploadSession> = self
            .http
            .post(format!(
                "{API_BASE}/accounts/{}/workers/scripts/{}/assets-upload-session",
                account.account_id, script_name
            ))
            .bearer_auth(&account.api_token)
            .json(&json!({ "manifest": Value::Object(manifest) }))
            .send()
            .context("starting assets upload session")?
            .json()
            .context("parsing upload session response")?;
        unwrap_envelope(resp, "assets upload session")
    }

    /// Upload the asset buckets. Returns the completion JWT.
    /// If `buckets` is empty the session JWT already is the completion token.
    pub fn upload_assets(
        &self,
        account: &TempAccount,
        session: &UploadSession,
        entries: &[AssetEntry],
    ) -> Result<String> {
        let buckets = session.buckets();
        if buckets.is_empty() {
            return Ok(session.jwt.clone());
        }

        let by_hash: HashMap<&str, &AssetEntry> =
            entries.iter().map(|e| (e.hash.as_str(), e)).collect();

        #[derive(Deserialize)]
        struct UploadResult {
            jwt: Option<String>,
        }

        let mut completion: Option<String> = None;
        let total = buckets.len();
        for (i, bucket) in buckets.iter().enumerate() {
            let mut form = multipart::Form::new();
            for hash in bucket {
                let entry = by_hash
                    .get(hash.as_str())
                    .ok_or_else(|| anyhow!("server requested unknown hash {hash}"))?;
                let mime = mime_guess::from_path(&entry.disk_path)
                    .first_or_octet_stream()
                    .to_string();
                let part = multipart::Part::text(entry.content_b64.clone())
                    .file_name(hash.clone())
                    .mime_str(&mime)
                    .context("setting part mime type")?;
                form = form.part(hash.clone(), part);
            }

            eprintln!("Uploading bucket {}/{} ({} file(s))...", i + 1, total, bucket.len());
            let resp: Envelope<UploadResult> = self
                .http
                .post(format!(
                    "{API_BASE}/accounts/{}/workers/assets/upload?base64=true",
                    account.account_id
                ))
                .bearer_auth(&session.jwt)
                .multipart(form)
                .send()
                .context("uploading assets")?
                .json()
                .context("parsing asset upload response")?;
            let r = unwrap_envelope(resp, "asset upload")?;
            if let Some(jwt) = r.jwt {
                completion = Some(jwt);
            }
        }

        completion.context("upload finished but no completion token was returned")
    }

    /// Deploy an assets-only Worker using the completion JWT.
    pub fn deploy_worker(
        &self,
        account: &TempAccount,
        script_name: &str,
        completion_jwt: &str,
    ) -> Result<()> {
        let metadata = json!({
            "assets": {
                "jwt": completion_jwt,
                "config": {
                    "html_handling": "auto-trailing-slash",
                    "not_found_handling": "404-page"
                }
            },
            "compatibility_date": "2025-01-01",
        });

        let form = multipart::Form::new().part(
            "metadata",
            multipart::Part::text(metadata.to_string())
                .mime_str("application/json")
                .context("setting metadata mime")?,
        );

        let resp: Envelope<Value> = self
            .http
            .put(format!(
                "{API_BASE}/accounts/{}/workers/scripts/{}",
                account.account_id, script_name
            ))
            .bearer_auth(&account.api_token)
            .multipart(form)
            .send()
            .context("deploying worker")?
            .json()
            .context("parsing deploy response")?;
        unwrap_envelope(resp, "worker deploy")?;
        Ok(())
    }

    /// Ensure the script is served on workers.dev.
    pub fn enable_workers_dev(&self, account: &TempAccount, script_name: &str) -> Result<()> {
        let resp: Envelope<Value> = self
            .http
            .post(format!(
                "{API_BASE}/accounts/{}/workers/scripts/{}/subdomain",
                account.account_id, script_name
            ))
            .bearer_auth(&account.api_token)
            .json(&json!({ "enabled": true, "previews_enabled": false }))
            .send()
            .context("enabling workers.dev for script")?
            .json()
            .context("parsing script subdomain response")?;
        unwrap_envelope(resp, "script workers.dev enable")?;
        Ok(())
    }

    /// Fetch the account's workers.dev subdomain (e.g. "example-name").
    pub fn get_subdomain(&self, account: &TempAccount) -> Result<String> {
        #[derive(Deserialize)]
        struct Sub {
            subdomain: Option<String>,
        }
        let resp: Envelope<Sub> = self
            .http
            .get(format!(
                "{API_BASE}/accounts/{}/workers/subdomain",
                account.account_id
            ))
            .bearer_auth(&account.api_token)
            .send()
            .context("fetching workers.dev subdomain")?
            .json()
            .context("parsing subdomain response")?;
        let r = unwrap_envelope(resp, "subdomain lookup")?;
        r.subdomain
            .context("account has no workers.dev subdomain registered")
    }
}
