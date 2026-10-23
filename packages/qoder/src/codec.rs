use crate::*;
use aes::cipher::{block_padding::Pkcs7, BlockEncryptMut, KeyIvInit};
use base64::{engine::general_purpose::STANDARD, Engine};
use md5::{Digest, Md5};
use rsa::{pkcs8::DecodePublicKey, Pkcs1v15Encrypt, RsaPublicKey};
const RSA:&str="-----BEGIN PUBLIC KEY-----\nMIGfMA0GCSqGSIb3DQEBAQUAA4GNADCBiQKBgQDA8iMH5c02LilrsERw9t6Pv5Nc\n4k6Pz1EaDicBMpdpxKduSZu5OANqUq8er4GM95omAGIOPOh+Nx0spthYA2BqGz+l\n6HRkPJ7S236FZz73In/KVuLnwI8JJ2CbuJap8kvheCCZpmAWpb/cPx/3Vr/J6I17\nXcW+ML9FoCI6AOvOzwIDAQAB\n-----END PUBLIC KEY-----";
const ALPHABET: &[u8; 64] = b"_doRTgHZBKcGVjlvpC,@aFSx#DPuNJme&i*MzLOEn)sUrthbf%Y^w.(kIQyXqWA!";
pub(crate) fn encode(input: &str) -> String {
    let standard = STANDARD.encode(input);
    let mut bytes = standard
        .bytes()
        .map(|byte| {
            if byte == b'=' {
                b'$'
            } else {
                ALPHABET[b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/"
                    .iter()
                    .position(|candidate| *candidate == byte)
                    .unwrap()]
            }
        })
        .collect::<Vec<_>>();
    swap(&mut bytes);
    String::from_utf8(bytes).unwrap()
}
#[cfg(test)]
pub(crate) fn decode(input: &str) -> PluginResult<String> {
    if input.len() > 8 * 1024 * 1024 || !input.is_ascii() {
        return Err(transport("Qoder returned an invalid encoded body."));
    }
    let mut bytes = input.as_bytes().to_vec();
    swap(&mut bytes);
    for byte in &mut bytes {
        *byte = if *byte == b'$' {
            b'='
        } else {
            let index = ALPHABET
                .iter()
                .position(|value| *value == *byte)
                .ok_or_else(|| transport("Qoder returned an invalid encoded body."))?;
            b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/"[index]
        };
    }
    String::from_utf8(
        STANDARD
            .decode(bytes)
            .map_err(|_| transport("Qoder returned an invalid encoded body."))?,
    )
    .map_err(|_| transport("Qoder returned invalid encoded text."))
}
fn swap(bytes: &mut Vec<u8>) {
    let third = bytes.len() / 3;
    if third == 0 {
        return;
    }
    let mut out = Vec::with_capacity(bytes.len());
    out.extend_from_slice(&bytes[bytes.len() - third..]);
    out.extend_from_slice(&bytes[third..bytes.len() - third]);
    out.extend_from_slice(&bytes[..third]);
    *bytes = out;
}
pub(crate) fn headers(
    url: &str,
    credentials: &Value,
    body: &str,
) -> PluginResult<BTreeMap<String, String>> {
    let machine = text(credentials, "machineId");
    if machine.is_empty() || machine.len() > 4096 || machine.chars().any(char::is_control) {
        return Err(signed_out());
    }
    let key = id()[..16].as_bytes().to_vec();
    let raw=json!({"uid":credentials.get("uid"),"aid":"","name":text(credentials,"name"),"email":text(credentials,"email"),"security_oauth_token":text(credentials,"access")}).to_string();
    let cipher = cbc::Encryptor::<aes::Aes128>::new_from_slices(&key, &key)
        .map_err(|_| invalid("The Qoder envelope key is invalid."))?;
    let info = STANDARD.encode(cipher.encrypt_padded_vec_mut::<Pkcs7>(raw.as_bytes()));
    let public = RsaPublicKey::from_public_key_pem(RSA)
        .map_err(|_| invalid("The Qoder envelope key is unavailable."))?;
    let wrapped = STANDARD.encode(
        public
            .encrypt(&mut rand::rngs::OsRng, Pkcs1v15Encrypt, &key)
            .map_err(|_| invalid("The Qoder account envelope could not be signed."))?,
    );
    let payload = STANDARD.encode(
        json!({"version":"v1","requestId":id(),"info":info,"cosyVersion":VERSION,"ideVersion":""})
            .to_string(),
    );
    let at = (now() / 1000).to_string();
    let url = url::Url::parse(url).map_err(|_| invalid("The Qoder request address is invalid."))?;
    let path = url.path().strip_prefix("/algo").unwrap_or(url.path());
    let signature = format!(
        "{:x}",
        Md5::digest(format!("{payload}\n{wrapped}\n{at}\n{body}\n{path}").as_bytes())
    );
    let arch = match std::env::consts::ARCH {
        "x86_64" => "x86_64",
        "aarch64" => "aarch64",
        "x86" => "x86",
        value => value,
    };
    let os = match std::env::consts::OS {
        "macos" => "darwin",
        "windows" => "win32",
        value => value,
    };
    Ok(BTreeMap::from([
        ("accept".into(), "application/json".into()),
        ("accept-encoding".into(), "identity".into()),
        ("content-type".into(), "application/json".into()),
        (
            "authorization".into(),
            format!("Bearer COSY.{payload}.{signature}"),
        ),
        ("cosy-business-product".into(), "app".into()),
        ("cosy-business-type".into(), "agent".into()),
        ("cosy-clientip".into(), machine.into()),
        ("cosy-clienttype".into(), "10".into()),
        ("cosy-data-policy".into(), "disagree".into()),
        ("cosy-date".into(), at),
        ("cosy-key".into(), wrapped),
        ("cosy-machineid".into(), machine.into()),
        ("cosy-machinetoken".into(), machine.into()),
        ("cosy-machinetype".into(), "5".into()),
        ("cosy-machineos".into(), format!("{arch}_{os}")),
        ("cosy-scene".into(), "app".into()),
        ("cosy-user".into(), text(credentials, "uid").into()),
        ("cosy-version".into(), VERSION.into()),
        ("login-version".into(), "v2".into()),
    ]))
}
#[cfg(test)]
#[path = "../test/codec.test.rs"]
mod tests;
