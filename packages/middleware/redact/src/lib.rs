// Privacy rules and transformations follow Magpie 19253f67 (MIT). The host
// knows only the extension contract; sensitive values remain in this process.
mod checks;
mod json;
mod protocol;
mod settings;
mod shape;
mod stream;
pub use protocol::{serve, Plugin};
use regex::Regex;
use serde::{Deserialize, Serialize};
pub use settings::{Rule, Settings};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, HashMap},
    sync::OnceLock,
};
pub use stream::StreamState;
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Finder {
    kind: String,
    pattern: String,
    bound: String,
    category: String,
    check: String,
    at: String,
    fix: String,
    groups: Option<Vec<String>>,
    group_checks: Vec<String>,
}
fn finders() -> &'static Vec<(Finder, Regex)> {
    static RULES: OnceLock<Vec<(Finder, Regex)>> = OnceLock::new();
    RULES.get_or_init(|| {
        serde_json::from_str::<Vec<Finder>>(include_str!("rules.json"))
            .unwrap()
            .into_iter()
            .map(|f| {
                let re = Regex::new(
                    &f.pattern
                        .replace(r"\b", r"(?-u:\b)")
                        .replace(r"\d", "[0-9]")
                        .replace(r"\s", r"[\t\n\f\r ]"),
                )
                .expect("Frozen masking rule is valid");
                (f, re)
            })
            .collect()
    })
}
#[derive(Clone)]
struct Span {
    a: usize,
    b: usize,
    kind: String,
    by: &'static str,
}
#[derive(Clone, Serialize, Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Hit {
    pub kind: String,
    pub by: String,
    pub placeholder: String,
    pub action: String,
    pub head: String,
    pub len: usize,
}
#[derive(Clone)]
struct Remembered {
    value: String,
    kind: String,
    shape: bool,
}
pub struct Engine {
    key: [u8; 32],
    values: BTreeMap<String, Remembered>,
    sent: HashMap<[u8; 32], Vec<Span>>,
    sent_old: HashMap<[u8; 32], Vec<Span>>,
    pub hits: Vec<Hit>,
    remember: bool,
}
impl Engine {
    pub fn new(key: [u8; 32]) -> Self {
        Self {
            key,
            values: BTreeMap::new(),
            sent: HashMap::new(),
            sent_old: HashMap::new(),
            hits: Vec::new(),
            remember: true,
        }
    }
    pub fn mask(&mut self, input: &str, settings: &Settings, is_json: bool) -> String {
        self.hits.clear();
        if is_json {
            json::mask(self, input, settings)
        } else {
            self.text(input, settings, false)
        }
    }
    pub fn preview(&mut self, input: &str, settings: &Settings) -> String {
        self.remember = false;
        self.hits.clear();
        let output = self.text(input, settings, false);
        self.remember = true;
        output
    }
    fn put(&mut self, kind: &str, value: &str, shaped: bool) -> String {
        let mut prefix = "";
        let mut raw = value;
        if shaped {
            let re = Regex::new(r"^[A-Za-z_][A-Za-z0-9_.-]*[ \t]*[=:][ \t]*()").unwrap();
            if let Some(c) = re.captures(value) {
                let i = c.get(1).unwrap().start();
                prefix = &value[..i];
                raw = &value[i..]
            }
        }
        let candidate = shaped.then(|| shape::shape(&self.key, kind, raw)).flatten();
        let candidate = candidate.filter(|s| self.values.get(s).is_none_or(|v| v.value == raw));
        let (key, stored, is_shape) = if let Some(s) = candidate {
            (s, raw, true)
        } else {
            prefix = "";
            (shape::tag(&self.key, kind, value), value, false)
        };
        if self.remember && !self.values.contains_key(&key) {
            if self.values.len() >= 200_000 {
                let keys: Vec<_> = self.values.keys().take(100_000).cloned().collect();
                for k in keys {
                    self.values.remove(&k);
                }
            }
            self.values.insert(
                key.clone(),
                Remembered {
                    value: stored.into(),
                    kind: kind.into(),
                    shape: is_shape,
                },
            );
        }
        format!("{prefix}{key}")
    }
    fn known(&mut self, s: &str, settings: &Settings) -> Vec<Span> {
        let key: [u8; 32] = Sha256::digest(
            [
                s.as_bytes(),
                &serde_json::to_vec(&settings.redact_kinds).unwrap(),
                &[
                    u8::from(settings.redact),
                    u8::from(settings.redact_personal),
                ],
            ]
            .concat(),
        )
        .into();
        if self.remember {
            if let Some(sp) = self.sent.get(&key).or_else(|| self.sent_old.get(&key)) {
                return sp.clone();
            }
        }
        let mut spans = Vec::new();
        for v in self.values.values() {
            if v.value.len() < 8 || v.kind == "TERM" || !covered_kind(settings, &v.kind) {
                continue;
            }
            for (a, _) in s.match_indices(&v.value) {
                let b = a + v.value.len();
                if standalone(&v.kind, s, a, b) {
                    spans.push(Span {
                        a,
                        b,
                        kind: v.kind.clone(),
                        by: "known",
                    })
                }
            }
        }
        if self.remember {
            if self.sent.len() >= 1 << 16 {
                self.sent_old = std::mem::take(&mut self.sent)
            }
            self.sent.insert(key, spans.clone());
        }
        spans
    }
    fn text(&mut self, raw: &str, settings: &Settings, escaped: bool) -> String {
        if raw.len() < 3 {
            return raw.into();
        }
        let view = if escaped {
            escaped_view(raw)
        } else {
            raw.to_owned()
        };
        let s = view.as_str();
        let mut found = Vec::new();
        for word in &settings.redact_words {
            let w = word.trim();
            if w.len() < 2 {
                continue;
            }
            let re = Regex::new(&format!(
                "{}{}",
                if settings.redact_words_any_case {
                    "(?i)"
                } else {
                    ""
                },
                regex::escape(w)
            ))
            .unwrap();
            for m in re.find_iter(s) {
                found.push(Span {
                    a: m.start(),
                    b: m.end(),
                    kind: "TERM".into(),
                    by: "word",
                });
            }
        }
        if settings.redact {
            for r in &settings.redact_rules {
                if let Ok(re) = r.compile() {
                    for c in re.captures_iter(s) {
                        let m = c.get(1).or_else(|| c.get(0)).unwrap();
                        if m.len() >= 4 {
                            found.push(Span {
                                a: m.start(),
                                b: m.end(),
                                kind: r.kind.clone(),
                                by: "yours",
                            });
                        }
                    }
                }
            }
        }
        let mut split = Vec::new();
        for (rule, re) in finders() {
            if !settings.covers(&rule.category) {
                continue;
            }
            for c in re.captures_iter(s) {
                if let Some(groups) = &rule.groups {
                    if !groups.is_empty() {
                        let mut took = false;
                        for (g, kind) in groups.iter().enumerate() {
                            if let Some(m) = c.get(g + 1) {
                                if takes(rule, s, m.start(), m.end(), &rule.group_checks[g]) {
                                    found.push(Span {
                                        a: m.start(),
                                        b: m.end(),
                                        kind: kind.clone(),
                                        by: "rule",
                                    });
                                    took = true;
                                }
                            }
                        }
                        if took {
                            split.push((c.get(1).unwrap().start(), c.get(0).unwrap().end()));
                        }
                        continue;
                    }
                }
                let m = (1..c.len())
                    .find_map(|i| c.get(i))
                    .or_else(|| c.get(0))
                    .unwrap();
                let Some((a, b)) = checks::fix(&rule.fix, s, m.start(), m.end()) else {
                    continue;
                };
                if takes(
                    rule,
                    s,
                    a,
                    b,
                    if rule.fix.is_empty() { &rule.check } else { "" },
                ) {
                    found.push(Span {
                        a,
                        b,
                        kind: rule.kind.clone(),
                        by: "rule",
                    });
                }
            }
        }
        found.retain(|f| f.kind != "EMAIL" || !split.iter().any(|(a, b)| f.a == *a && f.b <= *b));
        found.extend(self.known(s, settings));
        if escaped {
            found = outside_escapes(raw, found)
        }
        self.apply_spans(raw, settings, escaped, found)
    }
    fn apply_spans(
        &mut self,
        raw: &str,
        settings: &Settings,
        escaped: bool,
        mut found: Vec<Span>,
    ) -> String {
        let view = if escaped {
            escaped_view(raw)
        } else {
            raw.to_owned()
        };
        let s = view.as_str();
        found.retain(|f| checks::keep_span(s, f.a, f.b, &f.kind, f.by));
        let placeholders = Regex::new(r"\{\{[A-Z][A-Z0-9_]*_[a-z2-7]{8}\}\}").unwrap();
        let mut protected: Vec<_> = placeholders
            .find_iter(s)
            .map(|m| (m.start(), m.end()))
            .collect();
        for (k, v) in &self.values {
            if v.shape {
                protected.extend(s.match_indices(k).map(|(a, _)| (a, a + k.len())));
            }
        }
        found.retain(|f| !protected.iter().any(|(a, b)| f.a < *b && f.b > *a));
        found.sort_by(|a, b| a.a.cmp(&b.a).then_with(|| b.b.cmp(&a.b)));
        let mut out = String::new();
        let mut last = 0;
        let mut next = 0;
        for f in found {
            if f.a < next {
                continue;
            }
            next = f.b;
            let original = &raw[f.a..f.b];
            if settings
                .redact_allow
                .iter()
                .any(|a| a.trim().to_lowercase() == original.to_lowercase())
            {
                continue;
            }
            let value = if escaped {
                serde_json::from_str::<String>(&format!("\"{original}\""))
                    .unwrap_or_else(|_| original.into())
            } else {
                original.to_owned()
            };
            let action = settings
                .redact_actions
                .get(&f.kind)
                .filter(|s| matches!(s.as_str(), "warn" | "block"))
                .cloned()
                .unwrap_or_default();
            let placeholder = if action == "warn" {
                shape::tag(&self.key, &f.kind, &value)
            } else {
                self.put(&f.kind, &value, settings.redact_shape)
            };
            let chars: Vec<_> = value.chars().collect();
            let mut n = usize::from(chars.len() >= 2);
            if !checks::personal(&f.kind) && f.kind != "TERM" && chars.len() >= 16 {
                n = 0;
                for c in &chars {
                    if n >= 4 || !c.is_ascii_alphabetic() && !"-_".contains(*c) {
                        break;
                    }
                    n += 1;
                    if "-_".contains(*c) {
                        break;
                    }
                }
                n = n.max(1)
            }
            self.hits.push(Hit {
                kind: f.kind,
                by: f.by.into(),
                placeholder: placeholder.clone(),
                action: action.clone(),
                head: chars.iter().take(n).collect(),
                len: chars.len(),
            });
            if action == "warn" {
                continue;
            }
            out.push_str(&raw[last..f.a]);
            out.push_str(&placeholder);
            last = f.b;
        }
        out.push_str(&raw[last..]);
        out
    }
    pub fn restore(&self, input: &str, escaped: bool) -> String {
        let mut s = input.to_owned();
        for (k, v) in &self.values {
            if v.shape {
                s = s.replace(k, &v.value)
            }
        }
        let re = Regex::new(r"[A-Za-z][A-Za-z0-9_]*_[A-Za-z2-7]{8}").unwrap();
        let mut out = String::new();
        let mut last = 0;
        for m in re.find_iter(&s) {
            let (mut a, mut b) = (m.start(), m.end());
            if a < last
                || a > 0 && word(s.as_bytes()[a - 1])
                || b < s.len() && word(s.as_bytes()[b])
            {
                continue;
            }
            let body = m.as_str();
            let split = body.len() - 8;
            let k = format!(
                "{{{{{}{}}}}}",
                body[..split].to_ascii_uppercase(),
                body[split..].to_ascii_lowercase()
            );
            let Some(v) = self.values.get(&k) else {
                continue;
            };
            let (mut l, mut r, mut lc, mut rc) = (a, b, 0, 0);
            while l > 0 && b" \t".contains(&s.as_bytes()[l - 1]) {
                l -= 1
            }
            while lc < 2 && l > 0 && s.as_bytes()[l - 1] == b'{' {
                l -= 1;
                lc += 1
            }
            while r < s.len() && b" \t".contains(&s.as_bytes()[r]) {
                r += 1
            }
            while rc < 2 && r < s.len() && s.as_bytes()[r] == b'}' {
                r += 1;
                rc += 1
            }
            if lc == 2 || lc == 1 && rc >= 1 {
                a = l
            }
            if rc == 2 || rc == 1 && lc >= 1 {
                b = r
            }
            if a < last {
                continue;
            }
            out.push_str(&s[last..a]);
            if escaped {
                let encoded = serde_json::to_string(&v.value).unwrap();
                out.push_str(&encoded[1..encoded.len() - 1]);
            } else {
                out.push_str(&v.value)
            }
            last = b;
        }
        out.push_str(&s[last..]);
        out
    }
    pub fn restore_json(&self, s: &str) -> String {
        json::restore(self, s)
    }
}
fn word(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}
fn takes(r: &Finder, s: &str, a: usize, b: usize, check: &str) -> bool {
    let bs = s.as_bytes();
    a < b
        && !(a > 0 && r.bound.as_bytes().contains(&bs[a - 1])
            || b < bs.len() && r.bound.as_bytes().contains(&bs[b]))
        && checks::check(check, &s[a..b])
        && checks::at(&r.at, s, a, b)
}
fn covered_kind(s: &Settings, kind: &str) -> bool {
    if matches!(kind, "EMAIL" | "PHONE" | "ID_CARD" | "BANK_CARD") {
        return s.redact_personal;
    }
    let cats: Vec<_> = settings::CATEGORIES
        .iter()
        .filter(|c| c.3.contains(&kind))
        .collect();
    if cats.is_empty() {
        s.redact
    } else {
        cats.iter().any(|c| s.covers(c.0))
    }
}
fn standalone(kind: &str, s: &str, a: usize, b: usize) -> bool {
    let bs = s.as_bytes();
    let before = a.checked_sub(1).map(|i| bs[i]).unwrap_or(0);
    let after = bs.get(b).copied().unwrap_or(0);
    match kind {
        "PHONE" | "BANK_CARD" => !before.is_ascii_digit() && !after.is_ascii_digit(),
        "ID_CARD" | "IBAN" | "SSN" | "PASSPORT" | "SERIAL" => {
            !before.is_ascii_alphanumeric() && !after.is_ascii_alphanumeric()
        }
        "IP" | "MAC" => {
            !before.is_ascii_alphanumeric()
                && !b".:".contains(&before)
                && !after.is_ascii_alphanumeric()
                && after != b':'
                && !(after == b'.' && bs.get(b + 1).is_some_and(u8::is_ascii_digit))
        }
        "EMAIL" => {
            !before.is_ascii_alphanumeric()
                && !b"._%+-".contains(&before)
                && !after.is_ascii_alphanumeric()
                && !b"_%+".contains(&after)
                && !(b".-".contains(&after) && bs.get(b + 1).is_some_and(u8::is_ascii_alphanumeric))
        }
        _ => {
            !(before.is_ascii_alphanumeric() && bs[a].is_ascii_alphanumeric()
                || after.is_ascii_alphanumeric() && bs[b - 1].is_ascii_alphanumeric())
        }
    }
}
fn escaped_view(raw: &str) -> String {
    let mut b = raw.as_bytes().to_vec();
    let mut i = 0;
    while i + 1 < b.len() {
        if b[i] != b'\\' {
            i += 1;
            continue;
        }
        i += 1;
        match b[i] {
            b'n' | b'b' | b'f' => b[i] = b'\n',
            b'r' => b[i] = b'\r',
            b't' => b[i] = b'\t',
            b'u' => {
                let end = (i + 5).min(b.len());
                b[i..end].fill(b'\n');
                i += 4
            }
            _ => {}
        }
        i += 1;
    }
    String::from_utf8(b).unwrap()
}
fn outside_escapes(s: &str, mut found: Vec<Span>) -> Vec<Span> {
    let mut edges = vec![None; s.len() + 1];
    let mut i = 0;
    while i < s.len() {
        if s.as_bytes()[i] != b'\\' {
            i += 1;
            continue;
        }
        let end = (i + if s.as_bytes().get(i + 1) == Some(&b'u') {
            6
        } else {
            2
        })
        .min(s.len());
        for e in &mut edges[i + 1..end] {
            *e = Some((i, end))
        }
        i = end;
    }
    for f in &mut found {
        if let Some((_, end)) = edges[f.a] {
            f.a = end
        }
        if let Some((start, _)) = edges[f.b] {
            f.b = start
        }
    }
    found.retain(|f| f.a < f.b);
    found
}
