//! Unguessable tokens and PKCE helpers (RFC 7636), ticket T3.
//!
//! No `rand` dependency: the `state` value and the PKCE verifier are built from
//! two v4 UUIDs (122 random bits each) rendered as hex. That is 256 bits of
//! entropy in 64 characters, well inside PKCE's 43–128 character window and
//! inside the RFC 3986 unreserved set, so neither value needs escaping.

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use sha2::{Digest, Sha256};
use uuid::Uuid;

/// 64 hex characters of v4 UUID randomness.
pub fn random_token() -> String {
    let mut token = String::with_capacity(64);
    token.push_str(&Uuid::new_v4().simple().to_string());
    token.push_str(&Uuid::new_v4().simple().to_string());
    token
}

/// `BASE64URL(SHA256(verifier))`, unpadded — the S256 challenge.
pub fn pkce_challenge(verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

/// Percent-encode a query-string value, leaving RFC 3986 unreserved characters
/// alone. Used to build the consent URL, where `redirect_uri` contains `:` and
/// `/` and would otherwise be mis-parsed.
pub fn percent_encode(value: &str) -> String {
    let mut encoded = String::with_capacity(value.len());
    for byte in value.as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                encoded.push(char::from(*byte));
            }
            other => {
                encoded.push('%');
                encoded.push_str(&format!("{other:02X}"));
            }
        }
    }
    encoded
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn tokens_are_64_chars_and_unique() {
        let tokens: HashSet<String> = (0..64).map(|_| random_token()).collect();
        assert_eq!(tokens.len(), 64);
        assert!(tokens.iter().all(|token| token.len() == 64));
    }

    /// RFC 7636 appendix B test vector.
    #[test]
    fn pkce_s256_matches_the_rfc_test_vector() {
        let verifier = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
        assert_eq!(
            pkce_challenge(verifier),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
    }

    #[test]
    fn percent_encoding_escapes_the_redirect_uri_separators() {
        assert_eq!(
            percent_encode("https://strands.example/auth/google/callback"),
            "https%3A%2F%2Fstrands.example%2Fauth%2Fgoogle%2Fcallback"
        );
        assert_eq!(
            percent_encode("openid email profile"),
            "openid%20email%20profile"
        );
        assert_eq!(percent_encode("a-b_c.d~e"), "a-b_c.d~e");
    }
}
