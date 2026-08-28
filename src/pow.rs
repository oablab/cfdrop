//! Proof-of-work solver for Cloudflare temporary account provisioning.
//!
//! The challenge is a sequential SHA-256 checkpoint chain:
//! - checkpoint[0] = SHA-256(seed)
//! - for each of `k` segments, compute `g` sequential SHA-256 hashes from the
//!   previous checkpoint and append the result
//! - the solution is base64(standard) of the concatenated k+1 checkpoints

use anyhow::{bail, Context, Result};
use base64::Engine;
use sha2::{Digest, Sha256};

pub const MAX_WORK: u64 = 64_000_000;

pub struct Challenge {
    pub seed: String, // base64url, must decode to 32 bytes
    pub k: u64,
    pub g: u64,
}

fn sha256(input: &[u8]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(input);
    hasher.finalize().into()
}

/// Solve the challenge and return the base64 (standard alphabet) encoded
/// concatenation of all k+1 checkpoints.
pub fn solve(challenge: &Challenge) -> Result<String> {
    let seed = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(challenge.seed.trim_end_matches('='))
        .context("challenge seed is not valid base64url")?;
    if seed.len() != 32 {
        bail!("challenge seed must decode to 32 bytes, got {}", seed.len());
    }
    if challenge.k == 0 || challenge.g == 0 {
        bail!("challenge k and g must be positive integers");
    }
    if challenge.k.saturating_mul(challenge.g) > MAX_WORK {
        bail!(
            "challenge work factor k*g = {} exceeds the {} safety cap",
            challenge.k * challenge.g,
            MAX_WORK
        );
    }

    let k = challenge.k as usize;
    let mut checkpoints: Vec<u8> = Vec::with_capacity((k + 1) * 32);
    let mut hash = sha256(&seed);
    checkpoints.extend_from_slice(&hash);

    for _segment in 0..k {
        for _iter in 0..challenge.g {
            hash = sha256(&hash);
        }
        checkpoints.extend_from_slice(&hash);
    }

    Ok(base64::engine::general_purpose::STANDARD.encode(&checkpoints))
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine;

    fn seed_b64url(bytes: &[u8]) -> String {
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
    }

    #[test]
    fn solves_minimal_chain() {
        let seed = [7u8; 32];
        let challenge = Challenge {
            seed: seed_b64url(&seed),
            k: 2,
            g: 3,
        };
        let solution = solve(&challenge).unwrap();
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(&solution)
            .unwrap();
        // k+1 = 3 checkpoints of 32 bytes each
        assert_eq!(decoded.len(), 3 * 32);

        // Recompute by hand: c0 = sha(seed); c1 = sha^3(c0); c2 = sha^3(c1)
        let c0 = sha256(&seed);
        let mut h = c0;
        for _ in 0..3 {
            h = sha256(&h);
        }
        let c1 = h;
        for _ in 0..3 {
            h = sha256(&h);
        }
        let c2 = h;
        assert_eq!(&decoded[0..32], &c0);
        assert_eq!(&decoded[32..64], &c1);
        assert_eq!(&decoded[64..96], &c2);
    }

    #[test]
    fn rejects_bad_seed_length() {
        let challenge = Challenge {
            seed: seed_b64url(&[1u8; 16]),
            k: 1,
            g: 1,
        };
        assert!(solve(&challenge).is_err());
    }

    #[test]
    fn rejects_excessive_work() {
        let challenge = Challenge {
            seed: seed_b64url(&[1u8; 32]),
            k: 8_001,
            g: 8_000_000,
        };
        assert!(solve(&challenge).is_err());
    }

    #[test]
    fn rejects_zero_k_or_g() {
        let base = |k, g| Challenge {
            seed: seed_b64url(&[1u8; 32]),
            k,
            g,
        };
        assert!(solve(&base(0, 5)).is_err());
        assert!(solve(&base(5, 0)).is_err());
    }

    #[test]
    fn accepts_padded_base64url_seed() {
        // Some servers may include '=' padding; we strip it before decoding.
        let padded = format!(
            "{}=",
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode([9u8; 32])
        );
        let challenge = Challenge {
            seed: padded,
            k: 1,
            g: 1,
        };
        assert!(solve(&challenge).is_ok());
    }
}
