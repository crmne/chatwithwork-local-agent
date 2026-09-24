//! The device key (Ed25519) and DPoP-style proofs of possession.
//!
//! Proofs follow RFC 9449: a compact JWS with header
//! `{"typ":"dpop+jwt","alg":"EdDSA","jwk":{"kty":"OKP","crv":"Ed25519","x":...}}`
//! and claims `jti`, `htm`, `htu`, `iat`, plus `nonce` (the latest server
//! nonce) and `ath` (base64url SHA-256 of the access token) when present.

use anyhow::{Result, anyhow};
use base64::Engine;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use ring::rand::{SecureRandom, SystemRandom};
use ring::signature::{Ed25519KeyPair, KeyPair};
use serde_json::json;

pub struct DeviceKey {
    pair: Ed25519KeyPair,
    pkcs8: Vec<u8>,
}

impl DeviceKey {
    pub fn generate() -> Result<Self> {
        let rng = SystemRandom::new();
        let pkcs8 = Ed25519KeyPair::generate_pkcs8(&rng)
            .map_err(|_| anyhow!("generating the device key"))?;
        Self::from_pkcs8(pkcs8.as_ref())
    }

    pub fn from_pkcs8(pkcs8: &[u8]) -> Result<Self> {
        let pair = Ed25519KeyPair::from_pkcs8(pkcs8)
            .map_err(|_| anyhow!("the stored device key is invalid"))?;
        Ok(Self {
            pair,
            pkcs8: pkcs8.to_vec(),
        })
    }

    /// Standard base64 of the PKCS#8 document, for the secret store.
    pub fn to_stored(&self) -> String {
        STANDARD.encode(&self.pkcs8)
    }

    pub fn from_stored(s: &str) -> Result<Self> {
        let bytes = STANDARD
            .decode(s.trim())
            .map_err(|_| anyhow!("the stored device key is not valid base64"))?;
        Self::from_pkcs8(&bytes)
    }

    /// base64url (no padding) of the 32-byte public key: the JWK `x` value.
    pub fn public_key(&self) -> String {
        URL_SAFE_NO_PAD.encode(self.pair.public_key().as_ref())
    }

    /// RFC 7638 JWK thumbprint (base64url SHA-256), for display and binding.
    pub fn thumbprint(&self) -> String {
        let canonical = format!(
            r#"{{"crv":"Ed25519","kty":"OKP","x":"{}"}}"#,
            self.public_key()
        );
        let digest = ring::digest::digest(&ring::digest::SHA256, canonical.as_bytes());
        URL_SAFE_NO_PAD.encode(digest.as_ref())
    }

    pub fn sign(&self, message: &[u8]) -> Vec<u8> {
        self.pair.sign(message).as_ref().to_vec()
    }

    /// A DPoP proof for one request.
    pub fn proof(
        &self,
        method: &str,
        htu: &str,
        nonce: Option<&str>,
        access_token: Option<&str>,
    ) -> String {
        self.proof_at(method, htu, nonce, access_token, unix_now())
    }

    pub fn proof_at(
        &self,
        method: &str,
        htu: &str,
        nonce: Option<&str>,
        access_token: Option<&str>,
        iat: i64,
    ) -> String {
        let header = json!({
            "typ": "dpop+jwt",
            "alg": "EdDSA",
            "jwk": { "kty": "OKP", "crv": "Ed25519", "x": self.public_key() },
        });
        let mut claims = json!({
            "jti": random_token(16),
            "htm": method,
            "htu": htu,
            "iat": iat,
        });
        if let Some(nonce) = nonce {
            claims["nonce"] = json!(nonce);
        }
        if let Some(token) = access_token {
            let digest = ring::digest::digest(&ring::digest::SHA256, token.as_bytes());
            claims["ath"] = json!(URL_SAFE_NO_PAD.encode(digest.as_ref()));
        }
        let signing_input = format!(
            "{}.{}",
            URL_SAFE_NO_PAD.encode(header.to_string()),
            URL_SAFE_NO_PAD.encode(claims.to_string())
        );
        let signature = URL_SAFE_NO_PAD.encode(self.sign(signing_input.as_bytes()));
        format!("{signing_input}.{signature}")
    }
}

pub fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

/// `bytes` random bytes, base64url encoded.
pub fn random_token(bytes: usize) -> String {
    let mut buf = vec![0u8; bytes];
    SystemRandom::new()
        .fill(&mut buf)
        .expect("system randomness");
    URL_SAFE_NO_PAD.encode(buf)
}

/// Verify a proof. The daemon never needs this; it exists so tests (and
/// server implementers reading this code) can check the exact format.
pub fn verify_proof(proof: &str, public_key: &str) -> Result<serde_json::Value> {
    let mut parts = proof.split('.');
    let (Some(h), Some(c), Some(s), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return Err(anyhow!("not a compact JWS"));
    };
    let header: serde_json::Value = serde_json::from_slice(&URL_SAFE_NO_PAD.decode(h)?)?;
    if header["typ"] != "dpop+jwt" || header["alg"] != "EdDSA" || header["jwk"]["x"] != public_key {
        return Err(anyhow!("unexpected header {header}"));
    }
    let key_bytes = URL_SAFE_NO_PAD.decode(public_key)?;
    let key = ring::signature::UnparsedPublicKey::new(&ring::signature::ED25519, key_bytes);
    key.verify(format!("{h}.{c}").as_bytes(), &URL_SAFE_NO_PAD.decode(s)?)
        .map_err(|_| anyhow!("bad signature"))?;
    Ok(serde_json::from_slice(&URL_SAFE_NO_PAD.decode(c)?)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn proofs_verify_and_bind_token_and_nonce() {
        let key = DeviceKey::generate().unwrap();
        let restored = DeviceKey::from_stored(&key.to_stored()).unwrap();
        assert_eq!(key.public_key(), restored.public_key());

        let proof = key.proof_at(
            "GET",
            "https://example.com/local_agent",
            Some("n1"),
            Some("tok"),
            1000,
        );
        let claims = verify_proof(&proof, &key.public_key()).unwrap();
        assert_eq!(claims["htm"], "GET");
        assert_eq!(claims["htu"], "https://example.com/local_agent");
        assert_eq!(claims["nonce"], "n1");
        assert_eq!(claims["iat"], 1000);
        // ath = base64url(sha256("tok"))
        assert_eq!(claims["ath"], "GnZ0607njffhrEOak8P6jjyUV4TU3sn9jjARc4svHWI");

        let other = DeviceKey::generate().unwrap();
        assert!(verify_proof(&proof, &other.public_key()).is_err());
        let tampered = proof.replacen('.', ".x", 1);
        assert!(verify_proof(&tampered, &key.public_key()).is_err());
    }

    #[test]
    fn thumbprint_is_stable() {
        let key = DeviceKey::generate().unwrap();
        assert_eq!(key.thumbprint(), key.thumbprint());
        assert_eq!(key.thumbprint().len(), 43);
    }
}
