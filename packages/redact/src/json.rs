use crate::{Engine, Settings};
use serde_json::Value;
fn under(path: &[String], roots: &[Vec<String>]) -> bool {
    roots.iter().any(|r| path.starts_with(r))
}
pub(crate) fn protocol_value(k: &str, s: &str) -> bool {
    s.starts_with("data:") || kept(k)
}
fn kept(k: &str) -> bool {
    matches!(
        k,
        "signature"
            | "encrypted_content"
            | "data"
            | "thoughtSignature"
            | "thought_signature"
            | "model"
            | "id"
            | "tool_use_id"
            | "call_id"
            | "item_id"
            | "type"
            | "role"
            | "name"
            | "namespace"
            | "server_label"
            | "server_name"
            | "previous_response_id"
            | "prompt_cache_key"
            | "media_type"
            | "mime_type"
            | "mimeType"
            | "url"
            | "image_url"
            | "file_id"
            | "reasoning_effort"
            | "effort"
            | "stop_reason"
            | "finish_reason"
            | "status"
            | "object"
            | "event"
    ) || k == "ids"
        || k.ends_with("_id")
        || k.ends_with("_ids")
        || k.ends_with("Id")
        || k.ends_with("Ids")
}
#[derive(Default)]
struct Roots {
    signed: Vec<Vec<String>>,
    args: Vec<Vec<String>>,
    defs: Vec<Vec<String>>,
}
impl Roots {
    fn new(value: &Value) -> Self {
        let mut roots = Self::default();
        for key in [
            "tools",
            "tool_choice",
            "functions",
            "function_call",
            "toolConfig",
            "tool_config",
            "response_format",
            "text",
            "reasoning",
            "include",
            "output_format",
            "output_config",
            "generationConfig",
            "generation_config",
            "stream_options",
            "client_metadata",
        ] {
            if let Some(v) = value.get(key) {
                if key != "text" || v.is_object() {
                    roots.defs.push(vec![key.into()])
                }
            }
        }
        if let Some(items) = value["input"].as_array() {
            for (i, v) in items.iter().enumerate() {
                if matches!(
                    v["type"].as_str(),
                    Some("additional_tools" | "tool_search_output")
                ) {
                    roots
                        .defs
                        .push(vec!["input".into(), i.to_string(), "tools".into()])
                }
            }
        }
        roots.scan(value, &mut Vec::new(), "");
        roots
    }
    fn scan(&mut self, v: &Value, path: &mut Vec<String>, key: &str) {
        match v {
            Value::Object(obj) => {
                let typ = v["type"].as_str().unwrap_or("");
                if [
                    "signature",
                    "thoughtSignature",
                    "thought_signature",
                    "encrypted_content",
                ]
                .iter()
                .any(|k| {
                    v[k].as_str().is_some_and(|s| !s.is_empty())
                        && (matches!(*k, "thoughtSignature" | "thought_signature")
                            || matches!(typ, "thinking" | "reasoning")
                            || typ.starts_with("reasoning."))
                }) {
                    self.signed.push(path.clone());
                    return;
                }
                for (k, v) in obj {
                    path.push(k.clone());
                    if k == "input" && matches!(typ, "tool_use" | "server_tool_use")
                        || k == "args" && matches!(key, "functionCall" | "function_call")
                    {
                        self.args.push(path.clone())
                    } else {
                        self.scan(v, path, k)
                    }
                    path.pop();
                }
            }
            Value::Array(items) => {
                for (i, v) in items.iter().enumerate() {
                    path.push(i.to_string());
                    self.scan(v, path, key);
                    path.pop();
                }
            }
            _ => {}
        }
    }
    fn keep(&self, path: &[String], key: &str, s: &str) -> bool {
        s.starts_with("data:")
            || if under(path, &self.args) {
                false
            } else if under(path, &self.defs) {
                key != "description"
            } else {
                kept(key)
            }
    }
}
pub fn mask(engine: &mut Engine, body: &str, settings: &Settings) -> String {
    let Ok(value) = serde_json::from_str::<Value>(body) else {
        return engine.text(body, settings, false);
    };
    let roots = Roots::new(&value);
    let old_hits = engine.hits.len();
    let output = walk(body, |path, key, s| {
        if roots.keep(path, key, s) || under(path, &roots.signed) {
            s.to_owned()
        } else {
            engine.text(s, settings, matches!(key, "arguments" | "partial_json"))
        }
    })
    .unwrap_or_else(|| body.to_owned());
    if roots.signed.is_empty() {
        return output;
    }
    let mut pairs: Vec<_> = engine.hits[old_hits..]
        .iter()
        .filter(|h| h.action != "warn")
        .filter_map(|h| {
            engine
                .values
                .get(&h.placeholder)
                .map(|v| (v.value.clone(), h.placeholder.clone()))
        })
        .collect();
    pairs.sort_by_key(|p| std::cmp::Reverse(p.0.len()));
    pairs.dedup_by(|a, b| a.0 == b.0);
    walk(&output, |path, key, s| {
        if roots.keep(path, key, s) || !under(path, &roots.signed) {
            return s.into();
        }
        let mut t = s.to_owned();
        for (v, p) in &pairs {
            t = t.replace(v, p)
        }
        let known = engine.known(
            &t,
            &Settings {
                redact: settings.redact,
                ..Default::default()
            },
        );
        engine.apply_spans(&t, settings, false, known)
    })
    .unwrap_or(output)
}
pub fn restore(engine: &Engine, body: &str) -> String {
    let typ = serde_json::from_str::<Value>(body)
        .ok()
        .and_then(|v| v["type"].as_str().map(str::to_owned))
        .unwrap_or_default();
    walk(body, |_, key, s| {
        engine.restore(
            s,
            matches!(key, "arguments" | "partial_json")
                || key == "delta" && typ.contains("arguments"),
        )
    })
    .unwrap_or_else(|| engine.restore(body, false))
}
// Replacements touch only JSON string values; ordering, whitespace and original
// escapes are preserved when no replacement is required (including signed data).
pub(crate) fn walk(
    mut s: &str,
    mut f: impl FnMut(&[String], &str, &str) -> String,
) -> Option<String> {
    let original = s;
    let mut out = String::new();
    let mut path = Vec::new();
    value(&mut s, &mut out, &mut path, "", &mut f)?;
    let suffix = s.trim();
    if !suffix.is_empty() {
        return None;
    }
    out.push_str(s);
    if out == original {
        Some(original.into())
    } else {
        Some(out)
    }
}
fn ws<'a>(s: &mut &'a str, out: &mut String) {
    let n = s.len() - s.trim_start_matches([' ', '\t', '\r', '\n']).len();
    out.push_str(&s[..n]);
    *s = &s[n..];
}
fn token<'a>(s: &mut &'a str) -> Option<&'a str> {
    if !s.starts_with('"') {
        return None;
    }
    let b = s.as_bytes();
    let mut i = 1;
    while i < b.len() {
        match b[i] {
            b'\\' => i += 2,
            b'"' => {
                let t = &s[..i + 1];
                *s = &s[i + 1..];
                return Some(t);
            }
            _ => i += 1,
        }
    }
    None
}
fn value(
    s: &mut &str,
    out: &mut String,
    path: &mut Vec<String>,
    key: &str,
    f: &mut impl FnMut(&[String], &str, &str) -> String,
) -> Option<()> {
    ws(s, out);
    match s.as_bytes().first().copied()? {
        b'"' => {
            let t = token(s)?;
            let decoded: String = serde_json::from_str(t).ok()?;
            let changed = f(path, key, &decoded);
            if changed == decoded {
                out.push_str(t)
            } else {
                out.push_str(&serde_json::to_string(&changed).ok()?)
            }
        }
        b'{' => {
            out.push('{');
            *s = &s[1..];
            ws(s, out);
            if s.starts_with('}') {
                out.push('}');
                *s = &s[1..];
                return Some(());
            }
            loop {
                let t = token(s)?;
                let key: String = serde_json::from_str(t).ok()?;
                out.push_str(t);
                ws(s, out);
                if !s.starts_with(':') {
                    return None;
                }
                out.push(':');
                *s = &s[1..];
                path.push(key.clone());
                value(s, out, path, &key, f)?;
                path.pop();
                ws(s, out);
                match s.as_bytes().first()? {
                    b',' => {
                        out.push(',');
                        *s = &s[1..];
                        ws(s, out)
                    }
                    b'}' => {
                        out.push('}');
                        *s = &s[1..];
                        break;
                    }
                    _ => return None,
                }
            }
        }
        b'[' => {
            out.push('[');
            *s = &s[1..];
            ws(s, out);
            if s.starts_with(']') {
                out.push(']');
                *s = &s[1..];
                return Some(());
            }
            let mut i = 0;
            loop {
                path.push(i.to_string());
                value(s, out, path, key, f)?;
                path.pop();
                i += 1;
                ws(s, out);
                match s.as_bytes().first()? {
                    b',' => {
                        out.push(',');
                        *s = &s[1..]
                    }
                    b']' => {
                        out.push(']');
                        *s = &s[1..];
                        break;
                    }
                    _ => return None,
                }
            }
        }
        _ => {
            let n = s
                .find(|c: char| c.is_ascii_whitespace() || ",]}".contains(c))
                .unwrap_or(s.len());
            if n == 0 || serde_json::from_str::<Value>(&s[..n]).is_err() {
                return None;
            }
            out.push_str(&s[..n]);
            *s = &s[n..];
        }
    }
    Some(())
}
