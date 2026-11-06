//! Ark control-plane request signing, adapted from Magpie (MIT).
use chrono::{DateTime, Utc};
use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};

pub(crate) const HOST: &str = "open.volcengineapi.com";
pub(crate) const VERSION: &str = "2024-01-01";
pub(crate) const CONTENT_TYPE: &str = "application/json";

fn hash(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}
fn mac(key: &[u8], text: &str) -> Vec<u8> {
    let mut mac = Hmac::<Sha256>::new_from_slice(key).expect("HMAC accepts any key length");
    mac.update(text.as_bytes());
    mac.finalize().into_bytes().to_vec()
}
pub(crate) fn query(action: &str) -> String {
    let query = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("Action", action)
        .append_pair("Version", VERSION)
        .finish();
    query.replace('+', "%20")
}
pub(crate) fn sign(
    access_key: &str,
    secret: &str,
    region: &str,
    query: &str,
    body: &[u8],
    now: DateTime<Utc>,
) -> (String, String, String) {
    let at = now.format("%Y%m%dT%H%M%SZ").to_string();
    let date = now.format("%Y%m%d").to_string();
    let body_hash = hash(body);
    let signed = "content-type;host;x-content-sha256;x-date";
    let canonical = format!(
        "POST\n/\n{query}\ncontent-type:{CONTENT_TYPE}\nhost:{HOST}\nx-content-sha256:{body_hash}\nx-date:{at}\n\n{signed}\n{body_hash}"
    );
    let scope = format!("{date}/{region}/ark/request");
    let input = format!("HMAC-SHA256\n{at}\n{scope}\n{}", hash(canonical.as_bytes()));
    let mut key = mac(secret.as_bytes(), &date);
    for part in [region, "ark", "request"] {
        key = mac(&key, part);
    }
    let signature = hex::encode(mac(&key, &input));
    (
        format!("HMAC-SHA256 Credential={access_key}/{scope}, SignedHeaders={signed}, Signature={signature}"),
        at,
        body_hash,
    )
}

#[cfg(test)]
#[path = "../test/signing.test.rs"]
mod tests;
