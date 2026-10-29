use crate::settings::CATEGORIES;
use hmac::{Hmac, Mac};
use sha2::Sha256;
pub fn hmac(key: &[u8], value: &[u8]) -> Vec<u8> {
    let mut h = Hmac::<Sha256>::new_from_slice(key).expect("HMAC accepts any key size");
    h.update(value);
    h.finalize().into_bytes().to_vec()
}
pub fn tag(key: &[u8], kind: &str, value: &str) -> String {
    let digest = hmac(key, format!("{kind}\0{value}").as_bytes());
    let alphabet = b"abcdefghijklmnopqrstuvwxyz234567";
    let mut n = 0u64;
    for b in digest.iter().take(5) {
        n = n << 8 | u64::from(*b)
    }
    let code: String = (0..8)
        .rev()
        .map(|i| alphabet[((n >> (i * 5)) & 31) as usize] as char)
        .collect();
    format!("{{{{{kind}_{code}}}}}")
}
pub fn head(value: &str) -> usize {
    for p in [
        "sk-ant-api03-",
        "sk-ant-oat01-",
        "sk-ant-admin01-",
        "sk-proj-",
        "sk-svcacct-",
        "sk-or-v1-",
        "github_pat_",
    ] {
        if value.starts_with(p) {
            return p.len();
        }
    }
    if value.starts_with("AKIA") || value.starts_with("ASIA") {
        return 4;
    }
    for (i, c) in value.bytes().take(5).enumerate() {
        if matches!(c, b'-' | b'_') && i > 0 {
            return i + 1;
        }
        if !c.is_ascii_alphabetic() {
            break;
        }
    }
    0
}
pub fn shape(key: &[u8], kind: &str, value: &str) -> Option<String> {
    if kind == "TERM"
        || matches!(kind, "EMAIL" | "PHONE" | "ID_CARD" | "BANK_CARD")
        || CATEGORIES.iter().any(|c| c.3.contains(&kind))
        || value.len() < 8
        || !value
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"-_.".contains(&c))
    {
        return None;
    }
    let head = head(value);
    let rest = &value.as_bytes()[head..];
    let mut lower = true;
    let mut upper = true;
    for c in rest {
        match c {
            b'0'..=b'9' | b'-' | b'_' | b'.' => {}
            b'a'..=b'f' => upper = false,
            b'A'..=b'F' => lower = false,
            _ => {
                lower = false;
                upper = false
            }
        }
    }
    let seed = hmac(key, format!("shape\0{kind}\0{value}").as_bytes());
    let mut stream = Vec::new();
    let mut counter = 0u64;
    let mut i = 0;
    let mut made = 0;
    let mut out = value.as_bytes()[..head].to_vec();
    for c in rest {
        let set: &[u8] = match c {
            b'-' | b'_' | b'.' => b"",
            _ if lower => b"0123456789abcdef",
            _ if upper => b"0123456789ABCDEF",
            b'A'..=b'Z' => b"ABCDEFGHIJKLMNOPQRSTUVWXYZ",
            b'a'..=b'z' => b"abcdefghijklmnopqrstuvwxyz",
            _ => b"0123456789",
        };
        if set.is_empty() {
            out.push(*c);
            continue;
        }
        if i == stream.len() {
            stream = hmac(&seed, &counter.to_be_bytes());
            counter += 1;
            i = 0
        }
        out.push(set[stream[i] as usize % set.len()]);
        i += 1;
        made += 1;
    }
    let out = String::from_utf8(out).ok()?;
    (made >= 6 && out != value).then_some(out)
}
