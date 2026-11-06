use crate::{json, Engine};
use serde::{Deserialize, Serialize};
use serde_json::Value;
#[derive(Default, Serialize, Deserialize)]
pub struct StreamState {
    mode: u8,
    buffer: Vec<u8>,
    pending: Option<Held>,
    done: bool,
}
#[derive(Serialize, Deserialize)]
struct Held {
    context: String,
    path: Vec<String>,
    tail: String,
    full: String,
    cut: String,
}
impl StreamState {
    pub fn new(content_type: &str, encoding: &str) -> Self {
        Self {
            mode: if !encoding.is_empty() && !encoding.eq_ignore_ascii_case("identity") {
                3
            } else if content_type.contains("event-stream") {
                1
            } else if content_type.is_empty() {
                0
            } else {
                2
            },
            ..Default::default()
        }
    }
    pub fn push(&mut self, engine: &Engine, bytes: &[u8], finish: bool) -> Result<Vec<u8>, String> {
        if self.done {
            return Err("The masking stream has already finished.".into());
        }
        if self.mode == 0 {
            self.mode = if bytes.starts_with(b"event:")
                || bytes.starts_with(b"data:")
                || bytes.starts_with(b":")
            {
                1
            } else if bytes.is_empty() {
                0
            } else {
                2
            };
        }
        if self.mode == 3 {
            self.done = finish;
            return Ok(bytes.to_vec());
        }
        if self.buffer.len().saturating_add(bytes.len()) > 32 * 1024 * 1024 {
            return Err("The masking response exceeds its limit.".into());
        }
        self.buffer.extend_from_slice(bytes);
        let mut out = Vec::new();
        if self.mode == 1 {
            loop {
                let mut comments = 0;
                while comments < self.buffer.len() && b":\n\r".contains(&self.buffer[comments]) {
                    let Some(n) = self.buffer[comments..].iter().position(|b| *b == b'\n') else {
                        break;
                    };
                    if self.buffer[comments] == b'\r' && n != 1 {
                        break;
                    }
                    comments += n + 1;
                }
                if comments > 0 {
                    out.extend(self.buffer.drain(..comments));
                    continue;
                }
                let end = self
                    .buffer
                    .windows(2)
                    .position(|b| b == b"\n\n")
                    .map(|i| i + 2)
                    .into_iter()
                    .chain(
                        self.buffer
                            .windows(4)
                            .position(|b| b == b"\r\n\r\n")
                            .map(|i| i + 4),
                    )
                    .min();
                let Some(end) = end else { break };
                let ev: Vec<_> = self.buffer.drain(..end).collect();
                let ev =
                    std::str::from_utf8(&ev).map_err(|_| "Invalid UTF-8 in a masked response.")?;
                self.event(engine, ev, &mut out)?;
            }
        }
        if finish {
            self.done = true;
            if let Some(p) = self.pending.take() {
                out.extend_from_slice(p.full.as_bytes())
            }
            match std::str::from_utf8(&self.buffer) {
                Ok(s) => out.extend_from_slice(
                    if self.mode == 2 {
                        engine.restore_json(s)
                    } else {
                        engine.restore(s, false)
                    }
                    .as_bytes(),
                ),
                Err(_) if self.mode == 2 => out.extend_from_slice(&self.buffer),
                Err(_) => return Err("Invalid UTF-8 in a masked response.".into()),
            }
            self.buffer.clear();
        }
        Ok(out)
    }
    fn event(&mut self, e: &Engine, event: &str, out: &mut Vec<u8>) -> Result<(), String> {
        let Some((name, data, pre, post)) = split(event) else {
            if let Some(p) = self.pending.take() {
                out.extend_from_slice(p.full.as_bytes())
            }
            out.extend_from_slice(e.restore(event, false).as_bytes());
            return Ok(());
        };
        let Ok(value) = serde_json::from_str::<Value>(data) else {
            if let Some(p) = self.pending.take() {
                out.extend_from_slice(p.full.as_bytes())
            }
            out.extend_from_slice(e.restore(event, false).as_bytes());
            return Ok(());
        };
        let context = format!("{name}|{}", context(data));
        let mut carry = String::new();
        let mut path = Vec::new();
        if let Some(p) = self.pending.take() {
            let mut exists = false;
            json::walk(data, |at, _, s| {
                if at == p.path {
                    exists = true
                }
                s.into()
            });
            if p.context == context && exists {
                out.extend_from_slice(p.cut.as_bytes());
                carry = p.tail;
                path = p.path;
            } else {
                out.extend_from_slice(p.full.as_bytes())
            }
        }
        let typ = value["type"].as_str().unwrap_or("");
        let mut tail = String::new();
        let mut tail_path = Vec::new();
        let mut cut = String::new();
        let restored = json::walk(data, |at, key, s| {
            let s = if at == path {
                format!("{carry}{s}")
            } else {
                s.into()
            };
            if json::protocol_value(key, &s) {
                return s;
            }
            let escaped = matches!(key, "arguments" | "partial_json")
                || key == "delta" && typ.contains("arguments");
            let n = partial(e, &s);
            if n > 0 {
                tail = s[s.len() - n..].into();
                tail_path = at.to_vec();
                cut = e.restore(&s[..s.len() - n], escaped)
            }
            e.restore(&s, escaped)
        })
        .ok_or("Invalid JSON in a masked response.")?;
        let full = format!("{pre}{restored}{post}");
        if tail.is_empty() {
            out.extend_from_slice(full.as_bytes());
            return Ok(());
        }
        let cut_data = json::walk(&restored, |at, _, s| {
            if at == tail_path {
                cut.clone()
            } else {
                s.into()
            }
        })
        .ok_or("Invalid JSON in a masked response.")?;
        self.pending = Some(Held {
            context,
            path: tail_path,
            tail,
            full,
            cut: format!("{pre}{cut_data}{post}"),
        });
        Ok(())
    }
}
fn split(event: &str) -> Option<(&str, &str, &str, &str)> {
    let mut name = "";
    let mut found = None;
    let mut off = 0;
    for line in event.split_inclusive('\n') {
        let t = line.trim_end_matches(['\r', '\n']);
        if let Some(v) = t.strip_prefix("event:") {
            name = v.trim()
        }
        if t.starts_with("data:") {
            if found.is_some() {
                return None;
            }
            let start = if t.starts_with("data: ") { 6 } else { 5 };
            let data = &t[start..];
            if !data.starts_with(['{', '[']) {
                return None;
            }
            found = Some((off + start, off + t.len()));
        }
        off += line.len();
    }
    let (a, b) = found?;
    Some((name, &event[a..b], &event[..a], &event[b..]))
}
fn context(data: &str) -> String {
    let mut c = String::new();
    json::walk(data, |p, _, s| {
        if p.len() == 1 && matches!(p[0].as_str(), "type" | "item_id") {
            c.push_str(&format!("{}={s};", p[0]))
        }
        s.into()
    });
    for key in ["\"index\":", "\"output_index\":", "\"content_index\":"] {
        if let Some(i) = data.find(key) {
            let rest = &data[i + key.len()..];
            let n = rest
                .bytes()
                .take_while(|b| b.is_ascii_digit() || *b == b' ')
                .count();
            c.push_str(key);
            c.push_str(&rest[..n]);
            c.push(';')
        }
    }
    c
}
fn partial(e: &Engine, s: &str) -> usize {
    if let Some(mut i) = s.rfind('{') {
        if i > 0 && s.as_bytes()[i - 1] == b'{' {
            i -= 1
        }
        let tail = &s[i..];
        if tail.len() < 41 && !tail.contains("}}") {
            let body = tail.trim_start_matches('{').trim_start_matches([' ', '\t']);
            if body.is_empty() {
                return tail.len();
            }
            if body.as_bytes()[0].is_ascii_alphabetic() {
                let n = body.bytes().take_while(|b| crate::word(*b)).count();
                let rest = body[n..].trim_start_matches([' ', '\t']);
                if (rest.is_empty() || rest == "}")
                    && (n == body.len()
                        || regex::Regex::new(r"^[A-Za-z][A-Za-z0-9_]*_[A-Za-z2-7]{8}$")
                            .unwrap()
                            .is_match(&body[..n]))
                {
                    return tail.len();
                }
            }
        }
    }
    let n = s.bytes().rev().take_while(|b| crate::word(*b)).count();
    let word = &s[s.len() - n..];
    let mut best = 0;
    if !word.is_empty() && word.len() <= 33 && word.as_bytes()[0].is_ascii_uppercase() {
        let upper = word.to_ascii_uppercase();
        for v in e.values.values() {
            let kind = format!("{}_", v.kind);
            if kind.starts_with(&upper)
                || upper.strip_prefix(&kind).is_some_and(|r| {
                    r.len() <= 8
                        && r.bytes()
                            .all(|b| b.is_ascii_uppercase() || b"234567".contains(&b))
                })
            {
                best = n;
                break;
            }
        }
    }
    let whole = e
        .values
        .iter()
        .filter(|(_, v)| v.shape)
        .filter_map(|(k, _)| s.rfind(k).map(|i| i + k.len()))
        .max()
        .unwrap_or(0);
    for (k, v) in &e.values {
        if v.shape {
            for n in (best + 1..=k.len().saturating_sub(1).min(s.len() - whole)).rev() {
                if s.ends_with(&k[..n]) {
                    best = n;
                    break;
                }
            }
        }
    }
    best
}
