//! Webhook authenticity checks. GitHub signs the body (HMAC-SHA256 in
//! `X-Hub-Signature-256`); GitLab just echoes a shared token in
//! `X-Gitlab-Token`. Both comparisons are constant-time.

use hmac::{Hmac, Mac};
use sha2::Sha256;
use subtle::ConstantTimeEq;

/// Verify a GitHub `X-Hub-Signature-256` header (`sha256=<hex>`).
pub fn github_signature(secret: &str, body: &[u8], header: &str) -> bool {
    let Some(hex_sig) = header.strip_prefix("sha256=") else {
        return false;
    };
    let Ok(claimed) = hex::decode(hex_sig) else {
        return false;
    };
    let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes())
        .expect("hmac accepts any key length");
    mac.update(body);
    // verify_slice is constant-time.
    mac.verify_slice(&claimed).is_ok()
}

/// Verify a GitLab `X-Gitlab-Token` header against the configured token.
pub fn gitlab_token(expected: &str, provided: &str) -> bool {
    expected.as_bytes().ct_eq(provided.as_bytes()).into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn github_signature_roundtrip() {
        let secret = "it's a secret to everybody";
        let body = b"{\"zen\":\"speak like a human\"}";
        let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).unwrap();
        mac.update(body);
        let header = format!("sha256={}", hex::encode(mac.finalize().into_bytes()));

        assert!(github_signature(secret, body, &header));
        assert!(!github_signature(secret, b"tampered", &header));
        assert!(!github_signature("wrong secret", body, &header));
        assert!(!github_signature(secret, body, "sha256=nothex"));
        assert!(!github_signature(secret, body, "sha1=whatever"));
    }

    #[test]
    fn gitlab_token_compare() {
        assert!(gitlab_token("tok", "tok"));
        assert!(!gitlab_token("tok", "nope"));
        assert!(!gitlab_token("tok", ""));
    }
}
