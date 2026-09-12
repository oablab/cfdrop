//! Where a deploy goes: a throwaway preview account (the default) or the
//! user's own Cloudflare account.
//!
//! Own-account mode is opt-in and must be *explicit*: `--own`, `--account`,
//! or `CLOUDFLARE_ACCOUNT_ID`. A bare `CLOUDFLARE_API_TOKEN` in the
//! environment does NOT switch modes — many shells carry one for wrangler,
//! and silently sending a throwaway preview into a real account would be the
//! wrong surprise. The token itself is only ever read from the environment or
//! a file, never from argv (shell history, `ps`).

use anyhow::{bail, Context, Result};
use std::path::Path;

/// Everything `deploy`/`rm` need to know about the target, resolved before any
/// network call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Mode {
    /// Provision (or reuse) a temporary preview account.
    Temporary,
    /// Deploy into the user's own account with their token. `account_id` is
    /// `None` when it must be inferred from `GET /accounts`.
    Own {
        account_id: Option<String>,
        api_token: String,
        /// Where the token came from, for the deploy summary.
        token_source: &'static str,
    },
}

/// Raw inputs, separated from clap/env so the decision is unit-testable.
#[derive(Debug, Default, Clone)]
pub struct Inputs {
    pub own_flag: bool,
    pub temporary_flag: bool,
    pub account_flag: Option<String>,
    pub token_file: Option<String>,
    pub env_account_id: Option<String>,
    pub env_api_token: Option<String>,
}

impl Inputs {
    /// Read `CLOUDFLARE_ACCOUNT_ID` / `CLOUDFLARE_API_TOKEN` (the wrangler
    /// names, so an existing setup works unchanged). Empty values count as
    /// unset.
    pub fn from_env(
        own_flag: bool,
        temporary_flag: bool,
        account_flag: Option<String>,
        token_file: Option<String>,
    ) -> Self {
        let nonempty = |k: &str| std::env::var(k).ok().filter(|v| !v.trim().is_empty());
        Self {
            own_flag,
            temporary_flag,
            account_flag,
            token_file,
            env_account_id: nonempty("CLOUDFLARE_ACCOUNT_ID"),
            env_api_token: nonempty("CLOUDFLARE_API_TOKEN"),
        }
    }
}

/// Decide the mode. `read_token_file` is injected so tests do not touch disk.
pub fn resolve(inputs: &Inputs, read_token_file: impl Fn(&Path) -> Result<String>) -> Result<Mode> {
    if inputs.temporary_flag {
        if inputs.own_flag || inputs.account_flag.is_some() {
            bail!("--temporary cannot be combined with --own or --account");
        }
        return Ok(Mode::Temporary);
    }

    let wants_own =
        inputs.own_flag || inputs.account_flag.is_some() || inputs.env_account_id.is_some();
    if !wants_own {
        return Ok(Mode::Temporary);
    }

    let (api_token, token_source) = if let Some(path) = &inputs.token_file {
        let raw = read_token_file(Path::new(path))
            .with_context(|| format!("reading --token-file {path}"))?;
        let tok = raw.trim().to_string();
        if tok.is_empty() {
            bail!("--token-file {path} is empty");
        }
        (tok, "--token-file")
    } else if let Some(tok) = &inputs.env_api_token {
        (tok.clone(), "CLOUDFLARE_API_TOKEN")
    } else {
        bail!(
            "own-account mode was requested (--own / --account / CLOUDFLARE_ACCOUNT_ID) \
             but no API token was found. Set CLOUDFLARE_API_TOKEN or pass --token-file. \
             Not falling back to a temporary account."
        );
    };

    let account_id = inputs
        .account_flag
        .clone()
        .or_else(|| inputs.env_account_id.clone())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());

    Ok(Mode::Own {
        account_id,
        api_token,
        token_source,
    })
}

/// Given the accounts a token can see and an optional requested id, pick the
/// one to deploy into. Exactly one visible account is auto-selected; several
/// need an explicit choice; a requested id must be among the visible ones.
pub fn choose_account(
    visible: &[(String, String)],
    requested: Option<&str>,
) -> Result<(String, String)> {
    if let Some(id) = requested {
        return visible
            .iter()
            .find(|(vid, _)| vid == id)
            .cloned()
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "the token has no access to account {id}. It can see: {}",
                    describe(visible)
                )
            });
    }
    match visible.len() {
        0 => bail!("the token can see no accounts — check its Account Resources scope"),
        1 => Ok(visible[0].clone()),
        _ => bail!(
            "the token can see {} accounts; pick one with --account <id> (or \
             CLOUDFLARE_ACCOUNT_ID): {}",
            visible.len(),
            describe(visible)
        ),
    }
}

fn describe(visible: &[(String, String)]) -> String {
    if visible.is_empty() {
        return "(none)".into();
    }
    visible
        .iter()
        .map(|(id, name)| format!("{id} ({name})"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Own-account overwrite rule: a Worker that already exists may only be
/// replaced if cfdrop created it (carries the tag) or the user passed
/// `--force`. `existing_tags` is `None` when no Worker of that name exists.
pub fn may_overwrite(existing_tags: Option<&[String]>, force: bool, tag: &str) -> bool {
    match existing_tags {
        None => true,
        Some(_) if force => true,
        Some(tags) => tags.iter().any(|t| t == tag),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn no_file(_: &Path) -> Result<String> {
        bail!("no file in tests")
    }

    fn base() -> Inputs {
        Inputs::default()
    }

    #[test]
    fn default_is_temporary_even_with_token_in_env() {
        let mut i = base();
        i.env_api_token = Some("tok".into());
        assert_eq!(resolve(&i, no_file).unwrap(), Mode::Temporary);
    }

    #[test]
    fn own_flag_with_env_token_infers_account_later() {
        let mut i = base();
        i.own_flag = true;
        i.env_api_token = Some("tok".into());
        assert_eq!(
            resolve(&i, no_file).unwrap(),
            Mode::Own {
                account_id: None,
                api_token: "tok".into(),
                token_source: "CLOUDFLARE_API_TOKEN"
            }
        );
    }

    #[test]
    fn account_flag_alone_switches_to_own() {
        let mut i = base();
        i.account_flag = Some("abc".into());
        i.env_api_token = Some("tok".into());
        match resolve(&i, no_file).unwrap() {
            Mode::Own { account_id, .. } => assert_eq!(account_id.as_deref(), Some("abc")),
            m => panic!("{m:?}"),
        }
    }

    #[test]
    fn env_account_id_switches_to_own() {
        let mut i = base();
        i.env_account_id = Some("envacc".into());
        i.env_api_token = Some("tok".into());
        match resolve(&i, no_file).unwrap() {
            Mode::Own { account_id, .. } => assert_eq!(account_id.as_deref(), Some("envacc")),
            m => panic!("{m:?}"),
        }
    }

    #[test]
    fn account_flag_beats_env_account() {
        let mut i = base();
        i.account_flag = Some("flag".into());
        i.env_account_id = Some("env".into());
        i.env_api_token = Some("tok".into());
        match resolve(&i, no_file).unwrap() {
            Mode::Own { account_id, .. } => assert_eq!(account_id.as_deref(), Some("flag")),
            m => panic!("{m:?}"),
        }
    }

    #[test]
    fn own_without_token_errors_instead_of_falling_back() {
        let mut i = base();
        i.own_flag = true;
        let err = resolve(&i, no_file).unwrap_err().to_string();
        assert!(err.contains("no API token"), "{err}");
        assert!(err.contains("Not falling back"), "{err}");
    }

    #[test]
    fn token_file_wins_over_env_and_is_trimmed() {
        let mut i = base();
        i.own_flag = true;
        i.env_api_token = Some("envtok".into());
        i.token_file = Some("/x".into());
        let m = resolve(&i, |_| Ok("  filetok\n".into())).unwrap();
        match m {
            Mode::Own { api_token, token_source, .. } => {
                assert_eq!(api_token, "filetok");
                assert_eq!(token_source, "--token-file");
            }
            m => panic!("{m:?}"),
        }
    }

    #[test]
    fn empty_token_file_errors() {
        let mut i = base();
        i.own_flag = true;
        i.token_file = Some("/x".into());
        assert!(resolve(&i, |_| Ok("  \n".into())).is_err());
    }

    #[test]
    fn temporary_overrides_env_account() {
        let mut i = base();
        i.temporary_flag = true;
        i.env_account_id = Some("acc".into());
        i.env_api_token = Some("tok".into());
        assert_eq!(resolve(&i, no_file).unwrap(), Mode::Temporary);
    }

    #[test]
    fn temporary_conflicts_with_own_flags() {
        let mut i = base();
        i.temporary_flag = true;
        i.own_flag = true;
        assert!(resolve(&i, no_file).is_err());
    }

    fn accts(ids: &[&str]) -> Vec<(String, String)> {
        ids.iter().map(|s| (s.to_string(), format!("name-{s}"))).collect()
    }

    #[test]
    fn choose_single_account_automatically() {
        assert_eq!(choose_account(&accts(&["a"]), None).unwrap().0, "a");
    }

    #[test]
    fn choose_requires_explicit_when_several() {
        let err = choose_account(&accts(&["a", "b"]), None).unwrap_err().to_string();
        assert!(err.contains("--account"), "{err}");
        assert!(err.contains("a (name-a)"), "{err}");
    }

    #[test]
    fn choose_requested_must_be_visible() {
        assert_eq!(choose_account(&accts(&["a", "b"]), Some("b")).unwrap().0, "b");
        assert!(choose_account(&accts(&["a"]), Some("zzz")).is_err());
    }

    #[test]
    fn choose_none_visible_errors() {
        assert!(choose_account(&[], None).is_err());
    }

    #[test]
    fn overwrite_rule() {
        let ours = vec!["cfdrop".to_string()];
        let theirs = vec!["prod".to_string()];
        let none: Vec<String> = vec![];
        assert!(may_overwrite(None, false, "cfdrop"), "new name is free");
        assert!(may_overwrite(Some(&ours), false, "cfdrop"), "redeploying our own site");
        assert!(!may_overwrite(Some(&theirs), false, "cfdrop"), "someone else's Worker");
        assert!(!may_overwrite(Some(&none), false, "cfdrop"), "untagged Worker is not ours");
        assert!(may_overwrite(Some(&theirs), true, "cfdrop"), "--force overrides");
    }
}
