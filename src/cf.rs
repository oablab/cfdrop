//! Minimal Cloudflare API client for the temporary-account deploy flow.

use crate::manifest::AssetEntry;
use crate::pow::{self, Challenge};
use crate::state::{Credentials, TempAccount};
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
/// Worker tag stamped on every cfdrop deploy (see `deploy_worker`).
pub const CFDROP_TAG: &str = "cfdrop";

/// An account the token can see (`GET /accounts`).
#[derive(Deserialize, Debug, Clone)]
pub struct AccountSummary {
    pub id: String,
    pub name: String,
}

/// True when a Cloudflare error payload is the generic permission failure
/// (code 10000 "Authentication error") — the token exists but lacks the
/// permission group for this endpoint.
fn is_permission_error(errors: &Option<Vec<Value>>) -> bool {
    errors
        .as_ref()
        .map(|es| es.iter().any(|e| e.get("code").and_then(Value::as_i64) == Some(10000)))
        .unwrap_or(false)
}

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
            .user_agent(concat!("cfdrop/", env!("CARGO_PKG_VERSION")))
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
        creds: &Credentials,
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
                creds.account_id, script_name
            ))
            .bearer_auth(&creds.api_token)
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
        creds: &Credentials,
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
                    creds.account_id
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

    /// Deploy the Worker using the completion JWT. Without `auth`, deploys an
    /// assets-only Worker. With `auth` (base64 of "user:pass"), deploys a
    /// module Worker that enforces HTTP Basic Auth in front of the assets
    /// (`run_worker_first` so every request hits the guard).
    pub fn deploy_worker(
        &self,
        creds: &Credentials,
        script_name: &str,
        completion_jwt: &str,
        auth: Option<&str>,
    ) -> Result<()> {
        let mut assets_config = json!({
            "html_handling": "auto-trailing-slash",
            "not_found_handling": "404-page"
        });
        let mut metadata = json!({
            "assets": { "jwt": completion_jwt },
            "compatibility_date": "2025-01-01",
            // Marks the Worker as cfdrop-managed. In own-account mode a
            // redeploy is only allowed onto a script carrying this tag (or
            // with --force), so a typo in --name cannot clobber a hand-written
            // Worker in the user's account.
            "tags": [CFDROP_TAG],
        });

        let mut form = multipart::Form::new();

        if let Some(token) = auth {
            assets_config["run_worker_first"] = json!(true);
            metadata["main_module"] = json!("worker.mjs");
            metadata["bindings"] = json!([{ "type": "assets", "name": "ASSETS" }]);
            let script = auth_worker_script(token);
            form = form.part(
                "worker.mjs",
                multipart::Part::text(script)
                    .file_name("worker.mjs")
                    .mime_str("application/javascript+module")
                    .context("setting worker module mime")?,
            );
        }
        metadata["assets"]["config"] = assets_config;

        form = form.part(
            "metadata",
            multipart::Part::text(metadata.to_string())
                .mime_str("application/json")
                .context("setting metadata mime")?,
        );

        let resp: Envelope<Value> = self
            .http
            .put(format!(
                "{API_BASE}/accounts/{}/workers/scripts/{}",
                creds.account_id, script_name
            ))
            .bearer_auth(&creds.api_token)
            .multipart(form)
            .send()
            .context("deploying worker")?
            .json()
            .context("parsing deploy response")?;
        unwrap_envelope(resp, "worker deploy")?;
        Ok(())
    }

    /// Ensure the script is served on workers.dev.
    pub fn enable_workers_dev(&self, creds: &Credentials, script_name: &str) -> Result<()> {
        let resp: Envelope<Value> = self
            .http
            .post(format!(
                "{API_BASE}/accounts/{}/workers/scripts/{}/subdomain",
                creds.account_id, script_name
            ))
            .bearer_auth(&creds.api_token)
            .json(&json!({ "enabled": true, "previews_enabled": false }))
            .send()
            .context("enabling workers.dev for script")?
            .json()
            .context("parsing script subdomain response")?;
        unwrap_envelope(resp, "script workers.dev enable")?;
        Ok(())
    }

    /// Fetch the account's workers.dev subdomain (e.g. "example-name").
    pub fn get_subdomain(&self, creds: &Credentials) -> Result<String> {
        #[derive(Deserialize)]
        struct Sub {
            subdomain: Option<String>,
        }
        let resp: Envelope<Sub> = self
            .http
            .get(format!(
                "{API_BASE}/accounts/{}/workers/subdomain",
                creds.account_id
            ))
            .bearer_auth(&creds.api_token)
            .send()
            .context("fetching workers.dev subdomain")?
            .json()
            .context("parsing subdomain response")?;
        if !resp.success && is_permission_error(&resp.errors) {
            bail!(
                "the API token cannot read Workers on account {}: it needs the \
                 `Workers Scripts: Edit` permission (Account → Workers Scripts → Edit)",
                creds.account_id
            );
        }
        let r = unwrap_envelope(resp, "subdomain lookup")?;
        r.subdomain.ok_or_else(|| {
            anyhow!(
                "account {} has no workers.dev subdomain registered yet. Register one \
                 once in the dashboard (Workers & Pages → Overview → change subdomain) \
                 or with:\n  curl -X PUT -H \"Authorization: Bearer $CLOUDFLARE_API_TOKEN\" \\\n    \
                 {API_BASE}/accounts/{}/workers/subdomain -d '{{\"subdomain\":\"<name>\"}}'",
                creds.account_id, creds.account_id
            )
        })
    }

    // ----- own-account mode -------------------------------------------------

    /// Accounts this token is scoped to (`GET /accounts`). Works for both user
    /// tokens and account-owned tokens; `/user/*` endpoints do NOT (account
    /// tokens answer "Invalid API Token" there), which is why the preflight
    /// never touches them.
    pub fn list_accounts(&self, api_token: &str) -> Result<Vec<AccountSummary>> {
        let resp: Envelope<Vec<AccountSummary>> = self
            .http
            .get(format!("{API_BASE}/accounts?per_page=50"))
            .bearer_auth(api_token)
            .send()
            .context("listing accounts")?
            .json()
            .context("parsing accounts response")?;
        if !resp.success {
            bail!(
                "Cloudflare rejected the API token: {}",
                serde_json::to_string(&resp.errors.unwrap_or_default()).unwrap_or_default()
            );
        }
        Ok(resp.result.unwrap_or_default())
    }

    /// Tags of an existing Worker in the account, or `None` if no Worker of
    /// that name exists. Uses the list endpoint because `GET /scripts/{name}`
    /// downloads the script body rather than its metadata.
    pub fn script_tags(&self, creds: &Credentials, script_name: &str) -> Result<Option<Vec<String>>> {
        #[derive(Deserialize)]
        struct Script {
            id: String,
            // The list endpoint emits `"tags": null` for untagged Workers,
            // which `#[serde(default)]` alone does not accept.
            #[serde(default)]
            tags: Option<Vec<String>>,
        }
        let resp: Envelope<Vec<Script>> = self
            .http
            .get(format!(
                "{API_BASE}/accounts/{}/workers/scripts",
                creds.account_id
            ))
            .bearer_auth(&creds.api_token)
            .send()
            .context("listing Workers")?
            .json()
            .context("parsing Workers list")?;
        if !resp.success && is_permission_error(&resp.errors) {
            bail!(
                "the API token cannot list Workers on account {}: it needs the \
                 `Workers Scripts: Edit` permission",
                creds.account_id
            );
        }
        let scripts = unwrap_envelope(resp, "Workers list")?;
        Ok(scripts
            .into_iter()
            .find(|s| s.id == script_name)
            .map(|s| s.tags.unwrap_or_default()))
    }

    /// Delete a Worker (`DELETE /scripts/{name}`), used by `cfdrop rm`.
    pub fn delete_script(&self, creds: &Credentials, script_name: &str) -> Result<()> {
        let resp: Envelope<Value> = self
            .http
            .delete(format!(
                "{API_BASE}/accounts/{}/workers/scripts/{}",
                creds.account_id, script_name
            ))
            .bearer_auth(&creds.api_token)
            .send()
            .context("deleting Worker")?
            .json()
            .context("parsing delete response")?;
        if !resp.success {
            bail!(
                "delete failed: {}",
                serde_json::to_string(&resp.errors.unwrap_or_default()).unwrap_or_default()
            );
        }
        Ok(())
    }
}

/// Generate the Basic Auth guard Worker module. `token` is base64("user:pass")
/// (base64 alphabet only, safe to embed in a JS string literal).
fn auth_worker_script(token: &str) -> String {
    format!(
        r#"const EXPECTED = "Basic {token}";
export default {{
  async fetch(request, env) {{
    const got = request.headers.get("Authorization") || "";
    if (got.length !== EXPECTED.length || got !== EXPECTED) {{
      return new Response("Unauthorized", {{
        status: 401,
        headers: {{ "WWW-Authenticate": 'Basic realm="cfdrop", charset="UTF-8"' }},
      }});
    }}
    return env.ASSETS.fetch(request);
  }},
}};
"#
    )
}

#[cfg(test)]
mod tests {
    use super::auth_worker_script;

    #[test]
    fn auth_script_embeds_token_and_falls_through_to_assets() {
        let s = auth_worker_script("dXNlcjpwYXNz");
        assert!(s.contains(r#"const EXPECTED = "Basic dXNlcjpwYXNz";"#));
        assert!(s.contains("env.ASSETS.fetch(request)"));
        assert!(s.contains("WWW-Authenticate"));
        assert!(s.contains("status: 401"));
    }
}
