use regex::Regex;
use serde_json::Value;
use std::{net::IpAddr, sync::OnceLock};
fn dict() -> &'static Value {
    static DATA: OnceLock<Value> = OnceLock::new();
    DATA.get_or_init(|| serde_json::from_str(include_str!("dictionaries.json")).unwrap())
}
fn has(name: &str, value: &str) -> bool {
    dict()[name][value].as_bool() == Some(true)
}
fn digits(v: &str) -> bool {
    v.bytes().all(|b| b.is_ascii_digit())
}
fn letters(v: &str) -> bool {
    v.bytes().any(|b| b.is_ascii_alphabetic())
}
pub fn personal(k: &str) -> bool {
    matches!(k, "EMAIL" | "ID_CARD" | "PHONE" | "BANK_CARD")
        || crate::settings::CATEGORIES.iter().any(|c| c.3.contains(&k))
}
fn variable(v: &str) -> bool {
    !v.is_empty()
        && !b"$%{<[*".contains(&v.as_bytes()[0])
        && !v.trim_matches(['*', 'x', 'X', '.']).is_empty()
}
fn code(v: &str) -> bool {
    let l = v.to_lowercase();
    let Some((first, _)) = l.split_once('.') else {
        return false;
    };
    let last = l.rsplit('.').next().unwrap();
    has("codeObjects", first)
        || has("notUsers", last)
        || has("notNames", last)
        || has("publicHosts", last)
        || matches!(last, "login" | "username")
}
pub fn check(name: &str, v: &str) -> bool {
    match name {
        "" => true,
        "notAVariable" => variable(v),
        "secretValue" => {
            variable(v)
                && !v.starts_with("process.env")
                && !v.starts_with("os.")
                && letters(v)
                && v.bytes().any(|b| b.is_ascii_digit())
        }
        "realEmail" => {
            let host = v.rsplit('@').next().unwrap_or("").to_lowercase();
            let (stem, ext) = host.rsplit_once('.').unwrap_or(("", &host));
            !has("fileExt", ext)
                && !(has("scaleExt", ext)
                    && Regex::new(r"^[0-9]+(?:\.[0-9]+)?x$")
                        .unwrap()
                        .is_match(stem))
                && !fake_host(&host)
                && !host.starts_with("noreply")
                && !v.contains("noreply@")
        }
        "chineseID" => {
            let bs = v.as_bytes();
            bs.len() == 18
                && bs[..17].iter().all(u8::is_ascii_digit)
                && b"10X98765432"[bs[..17]
                    .iter()
                    .zip([7, 9, 10, 5, 8, 4, 2, 1, 6, 3, 7, 9, 10, 5, 8, 4, 2])
                    .map(|(b, w)| (b - b'0') as usize * w)
                    .sum::<usize>()
                    % 11]
                    == bs[17].to_ascii_uppercase()
        }
        "luhn" => {
            let ds: Vec<_> = v
                .bytes()
                .filter(u8::is_ascii_digit)
                .map(|b| b - b'0')
                .collect();
            (15..=19).contains(&ds.len())
                && ds
                    .iter()
                    .rev()
                    .enumerate()
                    .map(|(i, n)| {
                        let n = if i % 2 == 1 { n * 2 } else { *n };
                        u32::from(if n > 9 { n - 9 } else { n })
                    })
                    .sum::<u32>()
                    % 10
                    == 0
        }
        "ssnOK" => {
            let d = v.replace(['-', ' '], "");
            d.len() == 9
                && !matches!(&d[..3], "000" | "666")
                && !d.starts_with('9')
                && &d[3..5] != "00"
                && &d[5..] != "0000"
                && !matches!(d.as_str(), "123456789" | "078051120" | "219099999")
                && !d.bytes().all(|b| b == d.as_bytes()[0])
        }
        "passportOK" => {
            v.bytes().filter(u8::is_ascii_digit).count() >= 5
                && !v.bytes().all(|b| b == v.as_bytes()[0])
        }
        "dateOK" => date(v),
        "macOK" => {
            let s = v.to_ascii_lowercase().replace('-', ":");
            !matches!(
                s.as_str(),
                "00:00:00:00:00:00"
                    | "ff:ff:ff:ff:ff:ff"
                    | "00:11:22:33:44:55"
                    | "aa:bb:cc:dd:ee:ff"
                    | "01:23:45:67:89:ab"
                    | "12:34:56:78:9a:bc"
                    | "xx:xx:xx:xx:xx:xx"
            ) && s.contains(['a', 'b', 'c', 'd', 'e', 'f'])
        }
        "serialOK" => {
            v.len() >= 6
                && v.bytes().any(|b| b.is_ascii_digit())
                && !v.bytes().all(|b| b == v.as_bytes()[0])
        }
        "userOK" => {
            v.len() >= 2
                && !has("notUsers", &v.to_lowercase())
                && !has("notNames", &v.to_lowercase())
        }
        "hostOK" => {
            let s = v.trim_end_matches('.').to_lowercase();
            s.len() >= 2
                && !has("publicHosts", &s)
                && !s.starts_with("process.")
                && !s.starts_with("os.")
                && !s.starts_with("env.")
                && s.parse::<IpAddr>().is_err()
                && !fake_host(&s)
        }
        "lineUserOK" => check("userOK", v) && !code(v) && !has("chatWords", &v.to_lowercase()),
        "lineHostOK" => check("hostOK", v) && !code(v),
        "ipv6OK" => v
            .parse::<IpAddr>()
            .is_ok_and(|ip| ip.is_ipv6() && public_ip(ip)),
        "bucketOK" => v.len() >= 3 && !has("notBuckets", &v.to_lowercase()) && !digits(v),
        _ => false,
    }
}
fn fake_host(s: &str) -> bool {
    [
        "example.com",
        "example.org",
        "example.net",
        "localhost",
        ".test",
        ".invalid",
        ".example",
        ".localhost",
    ]
    .iter()
    .any(|x| s == x.trim_start_matches('.') || s.ends_with(x))
}
fn date(v: &str) -> bool {
    let ns: Vec<_> = v
        .split(|c: char| !c.is_ascii_digit())
        .filter(|s| !s.is_empty())
        .collect();
    let n = |s: &str| s.parse::<u32>().unwrap_or(0);
    let valid =
        |y, m, d| (1900..=2099).contains(&y) && (1..=12).contains(&m) && (1..=31).contains(&d);
    if ns.len() == 2 {
        let l = v.to_lowercase();
        for (i, m) in [
            "jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec",
        ]
        .iter()
        .enumerate()
        {
            if l.contains(m) {
                let (y, d) = if ns[0].len() == 4 {
                    (n(ns[0]), n(ns[1]))
                } else {
                    (n(ns[1]), n(ns[0]))
                };
                return valid(y, i as u32 + 1, d);
            }
        }
        return false;
    }
    if ns.len() != 3 {
        return false;
    }
    if ns[0].len() == 4 {
        return valid(n(ns[0]), n(ns[1]), n(ns[2]));
    }
    valid(n(ns[2]), n(ns[0]), n(ns[1])) || valid(n(ns[2]), n(ns[1]), n(ns[0]))
}
fn iban(v: &str) -> bool {
    let s = v.replace(' ', "");
    if s.len() < 4 || dict()["ibanLengths"][&s[..2]].as_u64() != Some(s.len() as u64) {
        return false;
    }
    let mut m = 0u32;
    for c in s[4..].bytes().chain(s[..4].bytes()) {
        m = match c {
            b'0'..=b'9' => (m * 10 + u32::from(c - b'0')) % 97,
            b'A'..=b'Z' => (m * 100 + u32::from(c - b'A') + 10) % 97,
            _ => return false,
        }
    }
    m == 1
}
pub fn fix(name: &str, s: &str, a: usize, mut b: usize) -> Option<(usize, usize)> {
    match name {
        "" => Some((a, b)),
        "homeAt" => {
            while b > a && s.as_bytes()[b - 1] == b'.' {
                b -= 1
            }
            (b > a && !s[a..b].starts_with('.') && !has("notNames", &s[a..b].to_lowercase()))
                .then_some((a, b))
        }
        "ibanAt" => loop {
            if iban(&s[a..b]) {
                return Some((a, b));
            }
            b = a + s[a..b].rfind(' ')?;
        },
        _ => None,
    }
}
fn public_ip(ip: IpAddr) -> bool {
    if has("resolvers", &ip.to_string()) {
        return false;
    }
    match ip {
        IpAddr::V4(ip) => {
            let x = u32::from(ip);
            if ip.is_private()
                || ip.is_loopback()
                || ip.is_link_local()
                || ip.is_multicast()
                || ip.is_unspecified()
            {
                return false;
            }
            [
                (0x00000000, 8),
                (0x64400000, 10),
                (0xc0000000, 24),
                (0xc0000200, 24),
                (0xc6120000, 15),
                (0xc6336400, 24),
                (0xcb007100, 24),
                (0xf0000000, 4),
            ]
            .iter()
            .all(|(p, n)| x >> (32 - n) != p >> (32 - n))
        }
        IpAddr::V6(ip) => {
            if let Some(ip) = ip.to_ipv4_mapped() {
                return public_ip(IpAddr::V4(ip));
            }
            let x = u128::from(ip);
            ip.octets()[0] & 0xe0 == 0x20 && (x >> 96) != 0x20010db8
        }
    }
}
pub fn at(name: &str, s: &str, a: usize, b: usize) -> bool {
    let v = &s[a..b];
    match name {
        "" => true,
        "keyAt" => {
            if variable(v) && letters(v) && (v.bytes().any(|b| b.is_ascii_digit()) || v.len() >= 16)
            {
                return true;
            }
            let before = s[..a].trim_end_matches([' ', '\t', '"', '\'', '“', '「', '\\']);
            let after = s[b..].trim_start_matches([' ', '\t']);
            v.len() >= 6
                && digits(v)
                && ["为", "為", "是", "：", ":", "=", "は", "는"]
                    .iter()
                    .any(|sep| before.ends_with(sep))
                && !dict()["quantityUnits"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|u| after.starts_with(u.as_str().unwrap()))
        }
        "notCode" => {
            if !Regex::new(r"^[A-Za-z_][A-Za-z0-9_]*(?:\.[A-Za-z0-9_]+)*$")
                .unwrap()
                .is_match(v)
            {
                return true;
            }
            let rest = s[b..].trim_start_matches([' ', '\t']);
            if rest.starts_with('(') {
                return false;
            }
            if a > 0 && b"\"'".contains(&s.as_bytes()[a - 1])
                || !s[..a].trim_end_matches([' ', '\t']).ends_with(':')
            {
                return true;
            }
            rest.is_empty() || !b";,)}]|?&<>".contains(&rest.as_bytes()[0])
        }
        "notUserinfo" => {
            let before = &s[..a];
            let i = before
                .rfind(|c: char| " \t\r\n/?#\"'<>@`".contains(c))
                .map_or(0, |i| i + 1);
            !(i >= 3
                && &s.as_bytes()[i - 3..i] == b"://"
                && s[..i - 3]
                    .chars()
                    .rev()
                    .take_while(|c| c.is_ascii_alphanumeric() || "+.-".contains(*c))
                    .any(|c| c.is_ascii_alphabetic()))
        }
        "ipv4At" => {
            let bs = s.as_bytes();
            if a > 0 {
                let c = bs[a - 1];
                if c.is_ascii_alphanumeric()
                    || b"._-=~^".contains(&c)
                    || c == b'/'
                        && a >= 2
                        && (bs[a - 2].is_ascii_alphanumeric() || b"._-".contains(&bs[a - 2]))
                {
                    return false;
                }
            }
            if b < bs.len()
                && (bs[b].is_ascii_alphanumeric()
                    || b"_-".contains(&bs[b])
                    || bs[b] == b'.' && b + 1 < bs.len() && bs[b + 1].is_ascii_digit())
            {
                return false;
            }
            let before = s[..a]
                .trim_end_matches([' ', '\t', ':', '=', '"', '\''])
                .to_lowercase();
            if ["version", "ver", "release", "build", "rev", "版本"]
                .iter()
                .any(|w| {
                    before.strip_suffix(w).is_some_and(|p| {
                        p.is_empty()
                            || !p
                                .as_bytes()
                                .last()
                                .is_some_and(|b| b.is_ascii_alphanumeric() || *b == b'_')
                    })
                })
            {
                return false;
            }
            v.parse().is_ok_and(public_ip)
        }
        _ => false,
    }
}
pub fn keep_span(s: &str, a: usize, b: usize, kind: &str, by: &str) -> bool {
    if matches!(by, "word" | "yours") {
        return true;
    }
    let bs = s.as_bytes();
    let hex = |c: u8| c.is_ascii_hexdigit() || c == b'-';
    let mut l = a;
    let mut r = b;
    while l > 0 && hex(bs[l - 1]) {
        l -= 1
    }
    while r < bs.len() && hex(bs[r]) {
        r += 1
    }
    while l < r && bs[l] == b'-' {
        l += 1
    }
    while r > l && bs[r - 1] == b'-' {
        r -= 1
    }
    if a >= l
        && b <= r
        && bs[l..r].iter().all(|b| hex(*b))
        && ((r - l >= 32 && !bs[l..r].contains(&b'-'))
            || Regex::new(r"^[0-9A-Fa-f]{8}(?:-[0-9A-Fa-f]{4}){3}-[0-9A-Fa-f]{12}$")
                .unwrap()
                .is_match(&s[l..r]))
    {
        if a > l || b < r {
            return false;
        }
        if by != "known"
            && !matches!(kind, "PASSWORD" | "API_KEY" | "TOKEN" | "PRIVATE_KEY")
            && (kind != "SECRET" || !credential(s, a))
        {
            return false;
        }
    }
    if !personal(kind) {
        return true;
    }
    let digit = |i: usize| bs.get(i).is_some_and(u8::is_ascii_digit);
    if digit(a) && a >= 2 && bs[a - 1] == b'.' && digit(a - 2)
        || digit(b - 1) && bs.get(b) == Some(&b'.') && digit(b + 1)
    {
        return false;
    }
    if !digits(&s[a..b]) {
        return true;
    }
    let after = s[b..].trim_start_matches([' ', '\t', '\n', '\r']);
    if !after.is_empty() && !b",}]".contains(&after.as_bytes()[0]) {
        return true;
    }
    let before = s[..a].trim_end_matches([' ', '\t', '\n', '\r']);
    if let Some(before) = before.strip_suffix(':') {
        return !before.trim_end().ends_with('"');
    }
    if before.ends_with(['[', ',']) {
        for c in before.bytes().rev() {
            if c == b'[' {
                return false;
            }
            if !c.is_ascii_digit() && !c.is_ascii_whitespace() && !b",.-+eE".contains(&c) {
                break;
            }
        }
    }
    true
}
fn credential(s: &str, a: usize) -> bool {
    let before = s[..a].trim_end_matches([' ', '\t', '"', '\'', '\\']);
    let Some(before) = before.strip_suffix([':', '=']) else {
        return false;
    };
    let before = before.trim_end_matches([' ', '\t', '"', '\'', '\\']);
    let name = before
        .rsplit(|c: char| !c.is_ascii_alphanumeric() && !"_.-".contains(c))
        .next()
        .unwrap_or("");
    let mut words = Vec::new();
    let mut word = String::new();
    let mut prev = ' ';
    for c in name.chars() {
        if "_.-".contains(c) || c.is_ascii_uppercase() && prev.is_ascii_lowercase() {
            if !word.is_empty() {
                words.push(std::mem::take(&mut word));
            }
        }
        if !"_.-".contains(c) {
            word.push(c.to_ascii_lowercase())
        }
        prev = c;
    }
    if !word.is_empty() {
        words.push(word)
    }
    !words.is_empty()
        && words != ["key"]
        && !matches!(
            words.last().map(String::as_str),
            Some("id" | "ids" | "uuid" | "guid")
        )
        && !words.iter().any(|s| {
            matches!(
                s.as_str(),
                "page"
                    | "next"
                    | "cursor"
                    | "continuation"
                    | "csrf"
                    | "xsrf"
                    | "trace"
                    | "request"
                    | "idempotency"
                    | "sync"
                    | "nonce"
                    | "hash"
                    | "digest"
                    | "checksum"
                    | "sha"
                    | "fingerprint"
                    | "etag"
            )
        })
}
