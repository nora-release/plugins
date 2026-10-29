//! Huawei SDK-HMAC-SHA256 and ES256 DPoP, adapted from
//! magpie-community/plugins/packages/codearts (MIT).
use crate::provider::{invalid, text, PluginResult};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use hmac::{Hmac, Mac};
use p256::ecdsa::{signature::Signer, Signature, SigningKey};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

pub(super) fn random(count: usize) -> PluginResult<Vec<u8>> {
    let mut value = vec![0u8; count];
    getrandom::getrandom(&mut value)
        .map_err(|_| invalid("The operating system random source is unavailable."))?;
    Ok(value)
}
pub(super) fn id(count: usize) -> PluginResult<String> {
    Ok(hex(&random(count)?))
}
pub(super) fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}
pub(super) fn digest(bytes: &[u8]) -> String {
    hex(&Sha256::digest(bytes))
}
pub(super) fn pkce() -> PluginResult<(String, String)> {
    let verifier = URL_SAFE_NO_PAD.encode(random(48)?);
    let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
    Ok((verifier, challenge))
}

pub(super) fn key() -> Value {
    let key = SigningKey::random(&mut rand_core::OsRng);
    let point = key.verifying_key().to_encoded_point(false);
    json!({"kty":"EC","crv":"P-256","x":URL_SAFE_NO_PAD.encode(point.x().unwrap()),"y":URL_SAFE_NO_PAD.encode(point.y().unwrap()),"d":URL_SAFE_NO_PAD.encode(key.to_bytes())})
}

pub(super) fn dpop(private: &Value, method: &str, url: &str, at: u64) -> PluginResult<String> {
    if text(private, "kty") != "EC" || text(private, "crv") != "P-256" {
        return Err(invalid(
            "The CodeArts sign-in key is invalid. Sign in again.",
        ));
    }
    let bytes = URL_SAFE_NO_PAD
        .decode(text(private, "d"))
        .map_err(|_| invalid("The CodeArts sign-in key is invalid."))?;
    let key = SigningKey::from_slice(&bytes)
        .map_err(|_| invalid("The CodeArts sign-in key is invalid."))?;
    let point = key.verifying_key().to_encoded_point(false);
    if text(private, "x") != URL_SAFE_NO_PAD.encode(point.x().unwrap())
        || text(private, "y") != URL_SAFE_NO_PAD.encode(point.y().unwrap())
    {
        return Err(invalid(
            "The CodeArts sign-in key does not match its public identity.",
        ));
    }
    let header = json!({"alg":"ES256","typ":"dpop+jwt","jwk":{"kty":"EC","crv":"P-256","x":text(private,"x"),"y":text(private,"y")}});
    let payload = json!({"htm":method,"htu":url,"iat":at/1000,"jti":id(32)?});
    let input = format!(
        "{}.{}",
        URL_SAFE_NO_PAD.encode(header.to_string()),
        URL_SAFE_NO_PAD.encode(payload.to_string())
    );
    let signature: Signature = key.sign(input.as_bytes());
    Ok(format!(
        "{input}.{}",
        URL_SAFE_NO_PAD.encode(signature.to_bytes())
    ))
}

pub(super) fn canonical_query(url: &url::Url) -> String {
    let mut pairs = url
        .query_pairs()
        .map(|(key, value)| (key.into_owned(), value.into_owned()))
        .collect::<Vec<_>>();
    pairs.sort();
    pairs
        .into_iter()
        .map(|(key, value)| format!("{}={}", encode(&key), encode(&value)))
        .collect::<Vec<_>>()
        .join("&")
}
pub(super) fn encode(value: &str) -> String {
    value
        .as_bytes()
        .iter()
        .map(|byte| {
            if byte.is_ascii_alphanumeric() || matches!(*byte, b'-' | b'_' | b'.' | b'~') {
                (*byte as char).to_string()
            } else {
                format!("%{byte:02X}")
            }
        })
        .collect()
}

pub(super) fn huawei(
    credentials: &Value,
    method: &str,
    url: &str,
    body: &[u8],
    extra: BTreeMap<String, String>,
    date: &str,
) -> PluginResult<BTreeMap<String, String>> {
    for field in ["access_key_id", "secret_access_key", "security_token"] {
        if text(credentials, field).is_empty()
            || text(credentials, field).chars().any(char::is_control)
        {
            return Err(invalid(
                "The CodeArts sign-in has no valid Huawei credential. Sign in again.",
            ));
        }
    }
    let url =
        url::Url::parse(url).map_err(|_| invalid("The CodeArts request address is invalid."))?;
    let host = url
        .host_str()
        .ok_or_else(|| invalid("The CodeArts request address has no host."))?;
    let host = if let Some(port) = url.port() {
        format!("{host}:{port}")
    } else {
        host.into()
    };
    let payload = digest(body);
    let mut headers = BTreeMap::from([
        ("host".into(), host),
        ("x-sdk-date".into(), date.into()),
        ("x-sdk-content-sha256".into(), payload.clone()),
        (
            "x-security-token".into(),
            text(credentials, "security_token").into(),
        ),
    ]);
    headers.extend(extra);
    if method != "GET" {
        headers.insert("content-type".into(), "application/json".into());
    }
    let names = headers
        .keys()
        .map(String::as_str)
        .collect::<Vec<_>>()
        .join(";");
    let lines = headers
        .iter()
        .map(|(key, value)| format!("{key}:{value}"))
        .collect::<Vec<_>>()
        .join("\n");
    let path = if url.path().ends_with('/') {
        url.path().to_owned()
    } else {
        format!("{}/", url.path())
    };
    let canonical = format!(
        "{method}\n{path}\n{}\n{lines}\n\n{names}\n{payload}",
        canonical_query(&url)
    );
    let input = format!("SDK-HMAC-SHA256\n{date}\n{}", digest(canonical.as_bytes()));
    let mut mac = Hmac::<Sha256>::new_from_slice(text(credentials, "secret_access_key").as_bytes())
        .map_err(|_| invalid("The CodeArts signing key is invalid."))?;
    mac.update(input.as_bytes());
    headers.insert(
        "authorization".into(),
        format!(
            "SDK-HMAC-SHA256 Access={},SignedHeaders={names},Signature={}",
            text(credentials, "access_key_id"),
            hex(&mac.finalize().into_bytes())
        ),
    );
    headers.remove("host");
    Ok(headers)
}

#[cfg(test)]
#[path = "../test/sign.test.rs"]
mod tests;
