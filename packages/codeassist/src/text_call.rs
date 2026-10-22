use super::*;
const MARKER: &str = "call:default_api:";
const QUOTE: &str = "<ctrl46>";
#[derive(Default)]
pub(crate) struct TextCalls {
    held: String,
}
impl TextCalls {
    pub fn flush(&mut self) -> Vec<Value> {
        if self.held.is_empty() {
            Vec::new()
        } else {
            vec![json!({"text":std::mem::take(&mut self.held)})]
        }
    }
    pub fn feed(&mut self, text: &str, names: &envelope::Names) -> Vec<Value> {
        let mut text = std::mem::take(&mut self.held) + text;
        let mut parts = Vec::new();
        while !text.is_empty() {
            let Some(index) = text.find(MARKER) else {
                let keep = (1..MARKER.len())
                    .rev()
                    .find(|n| text.ends_with(&MARKER[..*n]))
                    .unwrap_or(0);
                let end = text.len() - keep;
                if end > 0 {
                    parts.push(json!({"text":&text[..end]}));
                }
                self.held = text[end..].into();
                break;
            };
            if index > 0 {
                parts.push(json!({"text":&text[..index]}));
                text = text[index..].into();
            }
            match call(&text) {
                Call::Complete { name, args, end } => {
                    parts.push(json!({"functionCall":{"id":format!("call_{}",nora_ai::oauth_pkce::random_hex(12)),"name":names.restore(&name),"args":args}}));
                    text = text[end..].into();
                }
                Call::Incomplete if text.len() < 256 * 1024 => {
                    self.held = text;
                    break;
                }
                _ => {
                    let end = text[MARKER.len()..]
                        .find(MARKER)
                        .map(|index| index + MARKER.len())
                        .unwrap_or(text.len());
                    parts.push(json!({"text":&text[..end]}));
                    text = text[end..].into();
                }
            }
        }
        parts
    }
}
enum Call {
    Complete {
        name: String,
        args: Value,
        end: usize,
    },
    Incomplete,
    NotCall,
}
fn ident(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || b"_-.".contains(&byte)
}
fn call(text: &str) -> Call {
    let bytes = text.as_bytes();
    let mut cursor = MARKER.len();
    while cursor < bytes.len() && ident(bytes[cursor]) {
        cursor += 1;
    }
    if cursor == bytes.len() {
        return Call::Incomplete;
    }
    if cursor == MARKER.len() || bytes[cursor] != b'{' {
        return Call::NotCall;
    }
    let name = text[MARKER.len()..cursor].to_owned();
    let start = cursor;
    let mut depth = 0;
    let mut json_quote = false;
    let mut escape = false;
    while cursor < bytes.len() {
        if !json_quote && text[cursor..].starts_with(QUOTE) {
            let Some(end) = text[cursor + QUOTE.len()..].find(QUOTE) else {
                return Call::Incomplete;
            };
            cursor += 2 * QUOTE.len() + end;
            continue;
        }
        let byte = bytes[cursor];
        if json_quote {
            if escape {
                escape = false;
            } else if byte == b'\\' {
                escape = true;
            } else if byte == b'"' {
                json_quote = false;
            }
        } else {
            match byte {
                b'"' => json_quote = true,
                b'{' => depth += 1,
                b'}' => {
                    depth -= 1;
                    if depth == 0 {
                        return match args(&text[start + 1..cursor]) {
                            Some(args) => Call::Complete {
                                name,
                                args,
                                end: cursor + 1,
                            },
                            None => Call::NotCall,
                        };
                    }
                }
                _ => {}
            }
        }
        cursor += text[cursor..]
            .chars()
            .next()
            .map(char::len_utf8)
            .unwrap_or(1);
    }
    Call::Incomplete
}
fn next_key(text: &str) -> usize {
    let bytes = text.as_bytes();
    let (mut depth, mut quoted, mut escaped) = (0, false, false);
    let mut cursor = 0;
    while cursor < bytes.len() {
        let byte = bytes[cursor];
        if quoted {
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == b'"' {
                quoted = false;
            }
        } else {
            match byte {
                b'"' => quoted = true,
                b'{' | b'[' => depth += 1,
                b'}' | b']' => depth -= 1,
                b',' if depth == 0 => {
                    let mut next = cursor + 1;
                    while next < bytes.len() && bytes[next] == b' ' {
                        next += 1;
                    }
                    let start = next;
                    while next < bytes.len() && ident(bytes[next]) {
                        next += 1;
                    }
                    if next > start && bytes.get(next) == Some(&b':') {
                        return cursor;
                    }
                }
                _ => {}
            }
        }
        cursor += 1;
    }
    text.len()
}
fn args(body: &str) -> Option<Value> {
    let mut body = body.trim();
    if let Ok(value) = serde_json::from_str(&format!("{{{body}}}")) {
        return Some(value);
    }
    let mut values = serde_json::Map::new();
    while !body.is_empty() {
        let bytes = body.as_bytes();
        let mut end = 0;
        while end < bytes.len() && ident(bytes[end]) {
            end += 1;
        }
        if end == 0 || bytes.get(end) != Some(&b':') {
            return None;
        }
        let key = body[..end].to_owned();
        body = &body[end + 1..];
        let value;
        if let Some(rest) = body.strip_prefix(QUOTE) {
            let end = rest.find(QUOTE)?;
            value = json!(&rest[..end]);
            body = rest[end + QUOTE.len()..].trim_start();
            if !body.is_empty() {
                body = body.strip_prefix(',')?.trim_start();
            }
        } else {
            let end = next_key(body);
            let raw = body[..end].trim();
            value = serde_json::from_str(raw).unwrap_or_else(|_| json!(raw));
            body = body[end..].trim_start();
            if !body.is_empty() {
                body = body.strip_prefix(',')?.trim_start();
            }
        }
        values.insert(key, value);
    }
    Some(json!(values))
}
#[cfg(test)]
#[path = "../test/text_call.test.rs"]
mod tests;
