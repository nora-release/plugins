//! Tencent TC3 signing, adapted from Magpie (MIT) and Tencent's signing example.
use chrono::{DateTime, Utc};
use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};

pub(crate) const HOST: &str = "tokenhub.tencentcloudapi.com";
pub(crate) const VERSION: &str = "2026-03-22";
pub(crate) const CONTENT_TYPE: &str = "application/json; charset=utf-8";

pub(crate) fn hash(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}
fn mac(key: &[u8], text: &str) -> Vec<u8> {
    let mut mac = Hmac::<Sha256>::new_from_slice(key).expect("HMAC accepts any key length");
    mac.update(text.as_bytes());
    mac.finalize().into_bytes().to_vec()
}
pub(crate) fn sign(
    access_key: &str,
    secret: &str,
    service: &str,
    host: &str,
    action: &str,
    body: &[u8],
    now: DateTime<Utc>,
) -> String {
    let date = now.format("%Y-%m-%d").to_string();
    let signed = "content-type;host;x-tc-action";
    let canonical = format!(
        "POST\n/\n\ncontent-type:{CONTENT_TYPE}\nhost:{host}\nx-tc-action:{}\n\n{signed}\n{}",
        action.to_ascii_lowercase(),
        hash(body)
    );
    let scope = format!("{date}/{service}/tc3_request");
    let input = format!(
        "TC3-HMAC-SHA256\n{}\n{scope}\n{}",
        now.timestamp(),
        hash(canonical.as_bytes())
    );
    let mut key = mac(format!("TC3{secret}").as_bytes(), &date);
    for part in [service, "tc3_request"] {
        key = mac(&key, part);
    }
    let signature = hex::encode(mac(&key, &input));
    format!("TC3-HMAC-SHA256 Credential={access_key}/{scope}, SignedHeaders={signed}, Signature={signature}")
}

#[cfg(test)]
#[path = "../test/signing.test.rs"]
mod tests;
