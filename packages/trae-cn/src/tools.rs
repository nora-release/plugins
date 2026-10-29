//! Lossless tool-call adaptation for Trae's native and text-coded replies.
//! Adapted from magpie-community/plugins/packages/trae (MIT).
use crate::{id, transport, PluginError, PluginResult};
use nora_provider::tool_xml::{self, Parsed};
use regex::Regex;
use serde_json::{json, Map, Value};
use std::{
    collections::BTreeMap,
    sync::{Mutex, OnceLock},
};
const MAX_BUFFER: usize = 8 * 1024 * 1024;
const MAX_TEXT: usize = 32 * 1024 * 1024;
const MAX_CALLS: usize = 256;
const CUT_SHORT: &str = "Trae's model tool call was cut short; it cannot be used as an answer.";
const UNREADABLE: &str = "Trae's model tool call cannot be read; it cannot be used as an answer.";
const MARKS: &[&str] = &[
    "<tool_call>",
    "</tool_call>",
    "<toolcall>",
    "</toolcall>",
    "<｜DSML｜",
    "<|DSML|",
    "</｜DSML｜",
    "</|DSML|",
    "{\"reasoning_content\"",
];
fn marks() -> impl Iterator<Item = &'static str> {
    MARKS
        .iter()
        .chain(tool_xml::OPEN_MARKS)
        .chain(tool_xml::CLOSE_MARKS)
        .copied()
}
fn re(pattern: &'static str) -> Regex {
    static CACHE: OnceLock<Mutex<BTreeMap<&'static str, Regex>>> = OnceLock::new();
    let mut cache = CACHE
        .get_or_init(Default::default)
        .lock()
        .expect("regex cache");
    cache
        .entry(pattern)
        .or_insert_with(|| Regex::new(pattern).expect("static tool expression"))
        .clone()
}
fn object(value: Value) -> Option<Value> {
    value.is_object().then_some(value)
}
fn safe_numbers(value: &Value) -> bool {
    match value {
        Value::Number(n) if n.is_f64() => n
            .as_f64()
            .is_some_and(|v| v.is_finite() && (v.fract() != 0.0 || v.abs() <= 9007199254740991.0)),
        Value::Array(items) => items.iter().all(safe_numbers),
        Value::Object(items) => items.values().all(safe_numbers),
        _ => true,
    }
}
fn parse(s: &str) -> Option<Value> {
    serde_json::from_str(s).ok().filter(safe_numbers)
}
fn strict_args(s: &str) -> PluginResult<Value> {
    parse(s).ok_or_else(|| transport(CUT_SHORT))
}
fn is_done(s: &str) -> bool {
    !s.is_empty() && parse(s).is_some()
}
fn escape_raw(s: &str) -> String {
    let (mut output, mut quoted, mut escaped) = (String::new(), false, false);
    for ch in s.chars() {
        if quoted && !escaped && ch < ' ' {
            output.push_str(&match ch {
                '\n' => "\\n".into(),
                '\r' => "\\r".into(),
                '\t' => "\\t".into(),
                _ => format!("\\u{:04x}", ch as u32),
            });
            continue;
        }
        if quoted && ch == '\\' && !escaped {
            escaped = true;
        } else {
            if ch == '"' && !escaped {
                quoted = !quoted;
            }
            escaped = false;
        }
        output.push(ch);
    }
    output
}
fn loose(s: &str) -> Option<Value> {
    parse(s)
        .and_then(object)
        .or_else(|| parse(&escape_raw(s)).and_then(object))
}
fn fix_escapes(s: &str) -> String {
    let mut out = String::new();
    let mut chars = s.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch != '\\' {
            out.push(ch);
            continue;
        }
        match chars.peek().copied() {
            Some('\'') => {
                chars.next();
                out.push('\'');
            }
            Some('"' | '\\' | '/' | 'b' | 'f' | 'n' | 'r' | 't') => {
                out.push('\\');
                out.push(chars.next().unwrap());
            }
            Some('u') => {
                let mut look = chars.clone();
                look.next();
                if look.take(4).count() == 4
                    && chars.clone().skip(1).take(4).all(|c| c.is_ascii_hexdigit())
                {
                    out.push('\\');
                    out.push(chars.next().unwrap());
                } else {
                    out.push_str("\\\\");
                }
            }
            _ => out.push_str("\\\\"),
        }
    }
    out
}
fn quote_fix(s: &str) -> String {
    let mut out = String::new();
    let mut stack = Vec::new();
    let mut key = false;
    let mut at = 0;
    while at < s.len() {
        let ch = s[at..].chars().next().unwrap();
        if ch != '"' {
            match ch {
                '{' => {
                    stack.push('{');
                    key = true;
                }
                '[' => stack.push('['),
                '}' | ']' => {
                    stack.pop();
                }
                ',' => key = stack.last() == Some(&'{'),
                ':' => key = false,
                _ => {}
            }
            out.push(ch);
            at += ch.len_utf8();
            continue;
        }
        let is_key = key && stack.last() == Some(&'{');
        let array = stack.last() == Some(&'[');
        out.push('"');
        at += 1;
        while at < s.len() {
            let ch = s[at..].chars().next().unwrap();
            if ch == '\\' {
                out.push(ch);
                at += 1;
                if let Some(next) = s[at..].chars().next() {
                    out.push(next);
                    at += next.len_utf8();
                }
                continue;
            }
            if ch != '"' {
                out.push(ch);
                at += ch.len_utf8();
                continue;
            }
            let rest = &s[at + 1..];
            let ends = if is_key {
                rest.trim_start().starts_with(':')
            } else if array {
                re(r#"^\s*(?:[}\]]\s*)*(?:$|,\s*["{\[\d\-tfn])"#).is_match(rest)
            } else {
                re(r#"^\s*(?:[}\]]\s*)*(?:$|,\s*")"#).is_match(rest)
            };
            if ends {
                out.push(ch);
                at += 1;
                break;
            }
            out.push_str("\\\"");
            at += 1;
        }
        key = false;
    }
    out
}
fn repaired(s: &str) -> Option<Value> {
    if let Some(value) = loose(s) {
        return Some(value);
    }
    let text = escape_raw(s);
    let mut tries = Vec::new();
    if re(r#"^\s*\{\s*\\\""#).is_match(s) {
        let quoted = format!(
            "\"{}\"",
            s.trim()
                .replace('\n', "\\n")
                .replace('\r', "\\r")
                .replace('\t', "\\t")
        );
        if let Ok(decoded) = serde_json::from_str::<String>(&quoted) {
            tries.push(decoded.clone());
            tries.push(escape_raw(&decoded));
        }
        tries.push(s.replace("\\\"", "\""));
    }
    let fixed = fix_escapes(&text);
    tries.push(fixed.clone());
    tries.push(quote_fix(&fixed));
    tries.into_iter().find_map(|s| parse(&s).and_then(object))
}
fn call_json(s: &str) -> Option<Value> {
    repaired(s).filter(|v| v["name"].as_str().is_some_and(|s| !s.is_empty()))
}
fn object_end(s: &str, from: usize) -> Option<usize> {
    let mut depth = 0;
    let (mut quoted, mut escaped) = (false, false);
    let mut started = false;
    for (i, ch) in s[from..].char_indices() {
        if !started {
            if ch.is_whitespace() {
                continue;
            }
            if ch != '{' {
                return None;
            }
            started = true;
        }
        if quoted {
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                quoted = false;
            }
            continue;
        }
        match ch {
            '"' => quoted = true,
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(from + i + 1);
                }
            }
            _ => {}
        }
    }
    None
}
fn function(tool: &Value) -> &Value {
    tool.get("function").unwrap_or(tool)
}
fn tool<'a>(name: &str, tools: &'a [Value]) -> Option<&'a Value> {
    tools.iter().map(function).find(|v| v["name"] == name)
}
fn string_field(name: &str, key: &str, tools: &[Value]) -> bool {
    tool(name, tools)
        .or_else(|| {
            tool_named(name, tools)
                .ok()
                .and_then(|canonical| tool(&canonical, tools))
        })
        .and_then(|t| t.pointer("/parameters/properties"))
        .and_then(|p| p.get(key))
        .is_some_and(|p| p["type"] == "string")
}
fn field_value(raw: &str, name: &str, key: &str, tools: &[Value]) -> Value {
    if string_field(name, key, tools) {
        return json!(raw);
    }
    parse(raw.trim()).unwrap_or_else(|| json!(raw))
}
fn call_args(value: &Value) -> String {
    let arguments = value
        .get("arguments")
        .or_else(|| value.get("input"))
        .or_else(|| value.get("parameters"))
        .cloned()
        .unwrap_or_else(|| {
            let mut object = value.as_object().cloned().unwrap_or_default();
            for key in ["name", "type", "id"] {
                object.remove(key);
            }
            json!(object)
        });
    if let Some(text) = arguments.as_str() {
        if text.trim().is_empty() {
            "{}".into()
        } else {
            loose(text)
                .map(|v| v.to_string())
                .unwrap_or_else(|| text.into())
        }
    } else if arguments.is_null() {
        "{}".into()
    } else {
        arguments.to_string()
    }
}
fn quoted(text: &str) -> bool {
    if text.matches("```").count() % 2 == 1 {
        return true;
    }
    text.rsplit('\n')
        .next()
        .unwrap_or_default()
        .replace("```", "")
        .matches('`')
        .count()
        % 2
        == 1
}
fn close(s: &str, from: usize) -> Option<(usize, usize)> {
    re(r"</tool_?call>")
        .find_at(s, from)
        .map(|m| (m.start(), m.len()))
}
fn opener(s: &str) -> Option<(usize, usize, Option<String>)> {
    for prefix in re(r"<tool_?call").find_iter(s) {
        let after = &s[prefix.end()..];
        let mut length = 0;
        if after.starts_with('>') {
            length = 1;
        } else if after.starts_with(char::is_whitespace) {
            if let Some(end) = after.find('>') {
                if !after[..end].contains(['<', '{', '}']) {
                    length = end + 1;
                }
            }
        }
        if length > 0 {
            let attrs = &after[..length];
            let name = re(r#"\bname\s*=\s*"([^"]+)""#)
                .captures(attrs)
                .map(|c| c[1].to_string());
            return Some((prefix.start(), prefix.len() + length, name));
        }
        if after.trim_start().starts_with('{') {
            return Some((prefix.start(), prefix.len(), None));
        }
    }
    None
}
fn glm(s: &str, tools: &[Value]) -> Option<Value> {
    let input = re(r"</tool_?call>\s*$").replace(s, "");
    let index = input.find("<arg_key>");
    let name = input[..index.unwrap_or(input.len())].trim();
    if !re(r"^[\w.:-]{1,128}$").is_match(name) || index.is_none() && tool(name, tools).is_none() {
        return None;
    }
    let mut args = Map::new();
    for captures in re(r"<arg_key>\s*([\s\S]*?)\s*</arg_key>\s*<arg_value>([\s\S]*?)</arg_value>")
        .captures_iter(&input)
    {
        args.insert(
            captures[1].to_owned(),
            field_value(&captures[2], name, &captures[1], tools),
        );
    }
    if index.is_some() && args.is_empty() {
        return None;
    }
    Some(json!({"name":name,"arguments":args}))
}
fn parameters(s: &str, name: &str, tools: &[Value]) -> (Value, bool) {
    let pattern = re(r#"<parameter(?:\s+name\s*=\s*"([^"]+)"|=([^\s>]+))\s*>"#);
    let matches = pattern.captures_iter(s).collect::<Vec<_>>();
    let mut args = Map::new();
    let mut closed = !matches.is_empty();
    for (i, captures) in matches.iter().enumerate() {
        let start = captures.get(0).unwrap().end();
        let end = matches
            .get(i + 1)
            .and_then(|m| m.get(0))
            .map(|m| m.start())
            .unwrap_or(s.len());
        let raw = &s[start..end];
        let raw = if let Some(end) = raw.find("</parameter>") {
            &raw[..end]
        } else {
            closed = false;
            raw
        };
        let raw = raw
            .strip_prefix("\r\n")
            .or_else(|| raw.strip_prefix('\n'))
            .unwrap_or(raw);
        let raw = raw
            .strip_suffix("\r\n")
            .or_else(|| raw.strip_suffix('\n'))
            .unwrap_or(raw);
        let key = captures
            .get(1)
            .or_else(|| captures.get(2))
            .unwrap()
            .as_str();
        args.insert(key.into(), field_value(raw, name, key, tools));
    }
    (json!(args), closed)
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: String,
}
#[derive(Default, Debug)]
pub(crate) struct TextParts {
    pub text: String,
    pub reasoning: String,
    pub calls: Vec<ToolCall>,
    pub error: Option<PluginError>,
}
fn call(value: &Value) -> ToolCall {
    ToolCall {
        id: format!("call_{}", id()),
        name: value["name"].as_str().unwrap_or_default().into(),
        arguments: call_args(value),
    }
}
fn normalized_tool_name(name: &str) -> String {
    name.bytes()
        .filter(u8::is_ascii_alphanumeric)
        .map(|b| b.to_ascii_lowercase() as char)
        .collect()
}
fn tool_named(name: &str, tools: &[Value]) -> PluginResult<String> {
    if tool(name, tools).is_some() {
        return Ok(name.into());
    }
    let key = normalized_tool_name(name);
    let names = tools
        .iter()
        .map(function)
        .filter_map(|t| t["name"].as_str())
        .filter(|n| normalized_tool_name(n) == key)
        .collect::<Vec<_>>();
    if names.len() > 1 {
        return Err(transport("Trae returned an ambiguous tool alias."));
    }
    Ok(names.first().copied().unwrap_or(name).into())
}
pub(crate) fn normalize_call(mut call: ToolCall, tools: &[Value]) -> PluginResult<ToolCall> {
    if call.name.is_empty() || call.name.len() > MAX_BUFFER || call.arguments.len() > MAX_BUFFER {
        return Err(transport(UNREADABLE));
    }
    call.name = tool_named(&call.name, tools)?;
    if call.id.is_empty() {
        call.id = format!("call_{}", id());
    }
    if let Some(tool) = tool(&call.name, tools) {
        let props = tool.pointer("/parameters/properties");
        let mut value = loose(&call.arguments);
        let mut changed = false;
        for layer in 0..4 {
            let Some(object) = value.as_ref().and_then(Value::as_object) else {
                break;
            };
            if object.len() != 1 {
                break;
            }
            let (key, inner) = object.iter().next().unwrap();
            if !["arguments", "input", "parameters", "args"].contains(&key.as_str())
                || props.is_some_and(|p| p.get(key).is_some())
            {
                break;
            }
            let inner = if let Some(s) = inner.as_str() {
                loose(s)
            } else {
                Some(inner.clone())
            };
            let Some(inner) = inner.filter(Value::is_object) else {
                break;
            };
            if layer == 3 {
                return Err(transport(
                    "Trae returned too many nested tool argument wrappers.",
                ));
            }
            value = Some(inner);
            changed = true;
        }
        if changed {
            call.arguments = value.unwrap().to_string();
        }
    }
    Ok(call)
}
#[derive(Default)]
pub(crate) struct NativeCalls {
    calls: Vec<Native>,
}
struct Native {
    id: String,
    index: Option<u64>,
    name: String,
    arguments: String,
}
impl NativeCalls {
    pub fn push(&mut self, tc: &Value) -> PluginResult<()> {
        let f = tc
            .get("function")
            .or_else(|| tc.get("function_call"))
            .unwrap_or(tc);
        let name = f
            .get("name")
            .or_else(|| tc.get("tool_name"))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        let index = tc["index"].as_u64();
        let id = tc
            .get("id")
            .or_else(|| tc.get("tool_call_id"))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        let raw = f
            .get("arguments")
            .or_else(|| f.get("args"))
            .or_else(|| tc.get("params"))
            .or_else(|| tc.get("input"))
            .or_else(|| tc.get("parameters"));
        let arguments = raw
            .map(|v| {
                v.as_str()
                    .map(str::to_owned)
                    .unwrap_or_else(|| v.to_string())
            })
            .unwrap_or_default();
        if name.len() > MAX_BUFFER || arguments.len() > MAX_BUFFER || id.len() > 1024 {
            return Err(transport(UNREADABLE));
        }
        if name.is_empty() && index.is_none() && id.is_empty() && arguments.is_empty() {
            return Ok(());
        }
        let mut found = self.calls.iter().position(|old| {
            !id.is_empty() && old.id == id
                || index.is_some() && old.index == index && (id.is_empty() || old.id.is_empty())
        });
        if found.is_none() && id.is_empty() && index.is_none() {
            found = self.calls.iter().rposition(|old| {
                old.name == name
                    && (old.arguments == arguments
                        || !is_done(&old.arguments) && arguments.starts_with(&old.arguments))
            });
        }
        if found.is_none() && name.is_empty() && id.is_empty() {
            found = self.calls.iter().rposition(|old| !is_done(&old.arguments));
            if found.is_some_and(|i| {
                index.is_some() && self.calls[i].index.is_some() && index != self.calls[i].index
            }) {
                found = None;
            }
        }
        let Some(index_at) = found else {
            if name.is_empty() && id.is_empty() && index.is_none() {
                return Err(transport("Trae returned an unowned tool-call fragment."));
            }
            if self.calls.len() >= MAX_CALLS {
                return Err(transport("Trae returned too many tool calls."));
            }
            self.calls.push(Native {
                id,
                index,
                name,
                arguments,
            });
            return Ok(());
        };
        let old = &mut self.calls[index_at];
        if old.id.is_empty() && !id.is_empty() {
            old.id = id;
        }
        if old.name.is_empty() || !name.is_empty() && name.starts_with(&old.name) {
            old.name = name;
        }
        if old.index.is_none() {
            old.index = index;
        }
        if arguments.is_empty() || old.arguments.starts_with(&arguments) {
            return Ok(());
        }
        let next = if arguments.starts_with(&old.arguments)
            || is_done(&old.arguments)
            || resent(&old.arguments, &arguments)
        {
            arguments
        } else {
            format!("{}{arguments}", old.arguments)
        };
        if next.len() > MAX_BUFFER {
            return Err(transport(UNREADABLE));
        }
        old.arguments = next;
        Ok(())
    }
    pub fn take(&mut self, tools: &[Value]) -> PluginResult<Vec<ToolCall>> {
        let mut output = Vec::new();
        for native in std::mem::take(&mut self.calls) {
            if native.name.is_empty() {
                return Err(transport(CUT_SHORT));
            }
            let mut name = native.name;
            let mut args = native.arguments;
            let wrapped = if name.trim_start().starts_with('{') {
                loose(&name).or_else(|| name_call(&name, tools))
            } else if name.contains('{') {
                name_call(&name, tools)
            } else if name.contains("<arg_key>") {
                glm(&name, tools)
            } else {
                None
            };
            if let Some(wrapped) =
                wrapped.filter(|w| w["name"].as_str().is_some_and(|s| !s.is_empty()))
            {
                name = wrapped["name"].as_str().unwrap().into();
                let side = call_args(&wrapped);
                if args.trim().is_empty() || args.trim() == "{}" {
                    args = side;
                } else if let (Some(Value::Object(mut side)), Some(Value::Object(own))) =
                    (loose(&side), loose(&args))
                {
                    side.extend(own);
                    args = json!(side).to_string();
                }
            }
            if args.is_empty() {
                args = "{}".into();
            } else if !is_done(&args) {
                if let Some(value) = loose(&args) {
                    args = value.to_string();
                } else {
                    return Err(transport(CUT_SHORT));
                }
            }
            strict_args(&args)?;
            output.push(normalize_call(
                ToolCall {
                    id: native.id,
                    name,
                    arguments: args,
                },
                tools,
            )?);
        }
        Ok(output)
    }
}
fn resent(prev: &str, next: &str) -> bool {
    let key = re(r#"^\s*\{\s*"((?:[^"\\]|\\.)*)""#);
    next.trim_start().starts_with('{')
        && is_done(next)
        && !is_done(&format!("{prev}{next}"))
        && key
            .captures(prev)
            .zip(key.captures(next))
            .is_some_and(|(a, b)| a[1] == b[1])
}
fn decode_string(source: &str) -> String {
    serde_json::from_str::<String>(&format!("\"{}\"", escape_raw(source)))
        .unwrap_or_else(|_| source.into())
}
fn name_string(s: &str, mut at: usize, key: bool) -> (String, usize, bool) {
    at += 1;
    let mut source = String::new();
    while at < s.len() {
        let ch = s[at..].chars().next().unwrap();
        if ch == '\\' {
            source.push(ch);
            at += 1;
            if let Some(ch) = s[at..].chars().next() {
                source.push(ch);
                at += ch.len_utf8();
            }
            continue;
        }
        if ch == '<' && re(r"^</?[a-z_][\w-]*\s*>").is_match(&s[at..]) {
            break;
        }
        if ch == '"' {
            let rest = &s[at + 1..];
            let ends = key
                || re(r"^\s*(?:[,}\]]|$|</?[a-z_]+\s*>)").is_match(rest)
                || re(r#"^\s*"(?:[^"\\]|\\.)*"\s*(?::|</?[a-z_]+\s*>)"#).is_match(rest);
            if ends {
                return (decode_string(&source), at + 1, true);
            }
            source.push_str("\\\"");
            at += 1;
            continue;
        }
        source.push(ch);
        at += ch.len_utf8();
    }
    (decode_string(&source), at, false)
}
fn value_end(s: &str) -> usize {
    let mut end = s.len();
    for pattern in [
        r#""(?:(?:[^"\\<>]*)"\s*(?::|,|\s*</?[a-z_]+\s*>)|[^"\\<>]*</?[a-z_]+\s*>)"#,
        r"</?[a-z_]+\s*>",
        r"[}\]]",
    ] {
        if let Some(m) = re(pattern).find(s) {
            end = end.min(m.start());
        }
    }
    end
}
fn name_fields(body: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut at = 0;
    for _ in 0..64 {
        if at >= body.len() {
            break;
        }
        while let Some(m) = re(r"^(?:[\s,]+|</?[a-z_]+\s*>)").find(&body[at..]) {
            at += m.len();
        }
        if !body[at..].starts_with('"') {
            break;
        }
        let (key, next, _) = name_string(body, at, true);
        at = next;
        if key.is_empty() {
            break;
        }
        if let Some(m) = re(r"^\s*:\s*").find(&body[at..]) {
            at += m.len();
        }
        while let Some(m) = re(r"^</?[a-z_]+\s*>").find(&body[at..]) {
            at += m.len();
        }
        let value = if body[at..].starts_with('"') {
            let (value, next, closed) = name_string(body, at, false);
            at = next;
            if value.is_empty() && !closed {
                break;
            }
            value
        } else {
            let length = value_end(&body[at..]);
            let raw = &body[at..at + length];
            let mut value = raw
                .trim_start_matches(|c: char| c.is_whitespace() || c == ',' || c == ':')
                .trim_end_matches(|c: char| c.is_whitespace() || matches!(c, ',' | '}' | ']'))
                .to_owned();
            if value.ends_with('"') && !value.ends_with("\\\"") {
                value.pop();
            }
            value = value
                .trim_end_matches(|c: char| c.is_whitespace() || matches!(c, ',' | '}' | ']'))
                .into();
            at += length;
            if value.is_empty() {
                break;
            }
            value
        };
        out.push((key, value));
    }
    out
}
fn name_call(s: &str, tools: &[Value]) -> Option<Value> {
    let at = s.find('{')?;
    let head =
        re(r#"^\s*\{\s*"name"\s*:\s*"([^"\\]*)"\s*,\s*"(?:arguments|input|parameters)"\s*:\s*\{"#);
    let captures = head.captures(&s[at..])?;
    let name = &captures[1];
    tool(name, tools)?;
    let mut args = Map::new();
    for (key, value) in name_fields(&s[at + captures.get(0)?.end()..]) {
        args.insert(key.clone(), field_value(&value, name, &key, tools));
    }
    (!args.is_empty()).then(|| json!({"name":name,"arguments":args}))
}
fn dsml_at(s: &str) -> Option<usize> {
    re(r"<\s*/?\s*[|｜][\s|｜]*DSML[\s|｜]*")
        .find(s)
        .map(|m| m.start())
}
fn dsml_tag(s: &str) -> Option<usize> {
    re(r"^<\s*/?\s*[|｜][\s|｜]*DSML[\s|｜]*[^<>]*>")
        .find(s)
        .map(|m| m.end())
}
fn dsml_invoke(s: &str) -> Option<String> {
    re(r#"^<\s*[|｜][\s|｜]*DSML[\s|｜]*invoke\s+name\s*=\s*"([^"]+)"\s*>"#)
        .captures(s)
        .map(|c| c[1].to_owned())
}
fn dsml_args(s: &str) -> PluginResult<String> {
    let pattern = re(
        r#"<\s*[|｜][\s|｜]*DSML[\s|｜]*parameter\s+name\s*=\s*"([^"]+)"((?:\s+\w+\s*=\s*"[^"]*")*)\s*>([\s\S]*?)<\s*/\s*[|｜][\s|｜]*DSML[\s|｜]*parameter\s*>"#,
    );
    let mut args = Map::new();
    let mut consumed = Vec::new();
    for p in pattern.captures_iter(s) {
        let m = p.get(0).unwrap();
        consumed.push((m.start(), m.end()));
        let raw = &p[3];
        let value = if re(r#"string\s*=\s*"true""#).is_match(&p[2]) {
            json!(raw)
        } else {
            parse(raw.trim()).unwrap_or_else(|| json!(raw))
        };
        args.insert(p[1].to_owned(), value);
    }
    let mut remaining = s.to_owned();
    for (start, end) in consumed.into_iter().rev() {
        remaining.replace_range(start..end, "");
    }
    if dsml_at(&remaining).is_some() && remaining.contains("parameter") {
        return Err(transport(CUT_SHORT));
    }
    Ok(json!(args).to_string())
}
fn reason_prefix(s: &str) -> Option<usize> {
    let start = s.rfind('{')?;
    if s.len() - start > 96 {
        return None;
    }
    let tail = s[start + 1..].trim_start();
    let name = "\"reasoning_content\"";
    if name.starts_with(tail)
        || tail
            .strip_prefix(name)
            .is_some_and(|rest| rest.trim().is_empty() || rest.trim() == ":")
    {
        Some(start)
    } else {
        None
    }
}
enum Block {
    Wait,
    Text,
    Call(ToolCall, String),
}
pub(crate) struct TextTools {
    buffer: String,
    after: bool,
    tools: Vec<Value>,
    sent: String,
    mark: usize,
    finished: bool,
    calls: usize,
}
impl TextTools {
    pub fn new(tools: Vec<Value>) -> Self {
        Self {
            buffer: String::new(),
            after: false,
            tools,
            sent: String::new(),
            mark: 0,
            finished: false,
            calls: 0,
        }
    }
    fn block(&self, body: &str, named: Option<&str>, end: bool) -> PluginResult<Block> {
        if !end && body.trim().is_empty() {
            return Ok(Block::Wait);
        }
        let closing = close(body, 0);
        if let Some(name) = named {
            if closing.is_none() && !end {
                return Ok(Block::Wait);
            }
            let region = &body[..closing.map(|(i, _)| i).unwrap_or(body.len())];
            let rest = closing
                .map(|(i, n)| body[i + n..].to_owned())
                .unwrap_or_default();
            if region.trim_start().starts_with('{') {
                let value = repaired(region.trim()).ok_or_else(|| {
                    transport(if closing.is_some() {
                        UNREADABLE
                    } else {
                        CUT_SHORT
                    })
                })?;
                let call = if value
                    .get("name")
                    .is_some_and(|v| v.as_str().is_some_and(|s| !s.is_empty()))
                {
                    call(&value)
                } else {
                    ToolCall {
                        id: format!("call_{}", id()),
                        name: name.into(),
                        arguments: value.to_string(),
                    }
                };
                return Ok(Block::Call(call, rest));
            }
            if region.contains("<arg_key>") {
                let value = glm(&format!("{name}{region}"), &self.tools).ok_or_else(|| {
                    transport(if closing.is_some() {
                        UNREADABLE
                    } else {
                        CUT_SHORT
                    })
                })?;
                return Ok(Block::Call(call(&value), rest));
            }
            if !region.trim().is_empty() && !region.contains("<parameter") {
                return Err(transport(UNREADABLE));
            }
            let (args, closed) = parameters(region, name, &self.tools);
            if closing.is_none() && !closed {
                return Err(transport(CUT_SHORT));
            }
            return Ok(Block::Call(
                ToolCall {
                    id: format!("call_{}", id()),
                    name: name.into(),
                    arguments: args.to_string(),
                },
                rest,
            ));
        }
        if body.trim_start().starts_with('{') {
            if let Some(ph) = re(r#"^\s*\{\s*"name"\s*:\s*"([^"]+)"\s*\}?\s*>?\s*"#).captures(body)
            {
                let head = ph.get(0).unwrap();
                if body[head.end()..].starts_with("<parameter") {
                    if closing.is_none() && !end {
                        return Ok(Block::Wait);
                    }
                    let region = &body[head.end()..closing.map(|(i, _)| i).unwrap_or(body.len())];
                    let (args, closed) = parameters(region, &ph[1], &self.tools);
                    if closing.is_none() && !closed {
                        return Err(transport(CUT_SHORT));
                    }
                    return Ok(Block::Call(
                        ToolCall {
                            id: format!("call_{}", id()),
                            name: ph[1].to_owned(),
                            arguments: args.to_string(),
                        },
                        closing
                            .map(|(i, n)| body[i + n..].into())
                            .unwrap_or_default(),
                    ));
                }
            }
            if let Some(object_end) = object_end(body, 0) {
                if let Some(value) = call_json(&body[..object_end]) {
                    let tail = &body[object_end..];
                    if !end && value.as_object().is_some_and(|v| v.len() == 1) {
                        let start = tail
                            .trim_start()
                            .strip_prefix('>')
                            .unwrap_or(tail.trim_start())
                            .trim_start();
                        if "<parameter".starts_with(start) {
                            return Ok(Block::Wait);
                        }
                    }
                    let junk = re(r#"^[\s}\]"'>]*"#).find(tail).unwrap();
                    let next = &tail[junk.end()..];
                    if !end
                        && (next.is_empty()
                            || ["</tool_call>", "</toolcall>"]
                                .iter()
                                .any(|close| close.starts_with(next)))
                    {
                        return Ok(Block::Wait);
                    }
                    if let Some((0, n)) = close(next, 0) {
                        return Ok(Block::Call(call(&value), next[n..].into()));
                    }
                    if junk.as_str().trim().is_empty()
                        && (next.is_empty()
                            || next.starts_with('<')
                            || next.chars().next().is_some_and(char::is_whitespace))
                    {
                        return Ok(Block::Call(call(&value), tail.into()));
                    }
                    if closing.is_none() && end {
                        return Ok(Block::Call(call(&value), tail.into()));
                    }
                }
            }
            let mut from = 0;
            while let Some((at, n)) = close(body, from) {
                if let Some(value) = call_json(&body[..at]) {
                    return Ok(Block::Call(call(&value), body[at + n..].into()));
                }
                from = at + 1;
            }
            if !end {
                return Ok(Block::Wait);
            }
            let value = call_json(body).ok_or_else(|| {
                transport(if closing.is_some() {
                    UNREADABLE
                } else {
                    CUT_SHORT
                })
            })?;
            return Ok(Block::Call(call(&value), String::new()));
        }
        if !re(r"^\s*[\w.:-]{1,128}\s*(?:<|$)").is_match(body) {
            return Ok(Block::Text);
        }
        if closing.is_none() && !end {
            return Ok(Block::Wait);
        }
        let region = &body[..closing.map(|(i, _)| i).unwrap_or(body.len())];
        if let Some(value) = glm(region, &self.tools) {
            return Ok(Block::Call(
                call(&value),
                closing
                    .map(|(i, n)| body[i + n..].into())
                    .unwrap_or_default(),
            ));
        }
        if re(r"^\s*[\w.:-]{1,128}\s*<arg_key>").is_match(body) {
            return Err(transport(if closing.is_some() {
                UNREADABLE
            } else {
                CUT_SHORT
            }));
        }
        Ok(Block::Text)
    }
    fn append_call(&mut self, output: &mut TextParts, call: ToolCall) -> PluginResult<()> {
        self.calls += 1;
        if self.calls > MAX_CALLS {
            return Err(transport("Trae returned too many tool calls."));
        }
        strict_args(&call.arguments)?;
        output.calls.push(normalize_call(call, &self.tools)?);
        self.mark = self.sent.len() + output.text.len();
        self.after = true;
        Ok(())
    }
    fn context(&self, text: &str, head: &str) -> String {
        format!("{}{text}{head}", self.sent)
    }
    fn drop_leftovers(&mut self) {
        loop {
            let before = self.buffer.len();
            let trimmed = self.buffer.trim_start();
            let length = self.buffer.len() - trimmed.len();
            if length > 0 {
                self.buffer.drain(..length);
            }
            let xml_left = tool_xml::leftover_len(&self.buffer);
            if xml_left > 0 {
                self.buffer.drain(..xml_left);
            } else if let Some(m) = re(r"^</?tool_?calls?>").find(&self.buffer) {
                self.buffer.drain(..m.end());
            } else if let Some(length) = dsml_tag(&self.buffer) {
                if dsml_invoke(&self.buffer[..length]).is_none() {
                    self.buffer.drain(..length);
                }
            }
            if self.buffer.len() == before {
                break;
            }
        }
    }
    pub fn push(&mut self, input: &str, end: bool) -> PluginResult<TextParts> {
        if self.finished {
            return Err(transport("Trae emitted text after the tool stream ended."));
        }
        if self.buffer.len().saturating_add(input.len()) > MAX_BUFFER
            || self
                .sent
                .len()
                .saturating_add(self.buffer.len())
                .saturating_add(input.len())
                > MAX_TEXT
        {
            return Err(transport(
                "Trae's text tool stream exceeded its size limit.",
            ));
        }
        self.buffer.push_str(input);
        let mut output = TextParts::default();
        output.error = self.read(end, &mut output).err();
        if output.error.is_some() {
            self.buffer.clear();
        }
        if output.error.is_some() || end {
            self.finished = true;
        }
        self.sent.push_str(&output.text);
        Ok(output)
    }
    fn read(&mut self, end: bool, output: &mut TextParts) -> PluginResult<()> {
        loop {
            if self.after {
                self.drop_leftovers();
                if self.buffer.is_empty() || !end && marks().any(|m| m.starts_with(&self.buffer)) {
                    break;
                }
                if end
                    && self.buffer.len() > 1
                    && marks().any(|m| m.starts_with(self.buffer.trim_end()))
                {
                    self.buffer.clear();
                    break;
                }
                self.after = false;
            }
            let open = opener(&self.buffer);
            let d = dsml_at(&self.buffer);
            let x = tool_xml::at(&self.buffer);
            let r = re(r#"\{\s*"reasoning_content"\s*:"#)
                .find(&self.buffer)
                .map(|m| m.start());
            let stray = re(r#"[}\]"'>]*\s*</tool_?calls?>"#)
                .find(&self.buffer)
                .map(|m| (m.start(), m.end()));
            if let Some((at, to)) = stray.filter(|(at, _)| {
                [open.as_ref().map(|o| o.0), d, r, x]
                    .iter()
                    .all(|next| next.map_or(true, |n| *at < n))
            }) {
                let head = &self.buffer[..at];
                let context = self.context(&output.text, head);
                let tail = self.buffer[at..]
                    .chars()
                    .next()
                    .is_some_and(|c| matches!(c, '}' | ']' | '"' | '\'' | '>'))
                    || context[self.mark.min(context.len())..].trim().is_empty();
                if !tail || quoted(&context) {
                    output.text.push_str(&self.buffer[..to]);
                    self.buffer.drain(..to);
                    continue;
                }
                output.text.push_str(head);
                self.buffer.drain(..to);
                self.after = true;
                continue;
            }
            if let Some(at) = r.filter(|at| {
                open.as_ref().map_or(true, |o| *at < o.0)
                    && d.map_or(true, |d| *at < d)
                    && x.map_or(true, |x| *at < x)
            }) {
                if quoted(&self.context(&output.text, &self.buffer[..at])) {
                    output.text.push_str(&self.buffer[..at + 1]);
                    self.buffer.drain(..at + 1);
                    continue;
                }
                let ending = object_end(&self.buffer, at);
                if ending.is_none() && !end {
                    output.text.push_str(&self.buffer[..at]);
                    self.buffer.drain(..at);
                    break;
                }
                if let Some((to, value)) = ending
                    .and_then(|to| loose(&self.buffer[at..to]).map(|v| (to, v)))
                    .filter(|(_, v)| v["reasoning_content"].is_string())
                {
                    output.text.push_str(&self.buffer[..at]);
                    output
                        .reasoning
                        .push_str(value["reasoning_content"].as_str().unwrap());
                    self.buffer.drain(..to);
                    continue;
                }
                output.text.push_str(&self.buffer[..at + 1]);
                self.buffer.drain(..at + 1);
                continue;
            }
            if open.is_none() && d.is_none() && x.is_none() {
                break;
            }
            if let Some(at) =
                x.filter(|x| open.as_ref().is_none_or(|o| *x < o.0) && d.is_none_or(|d| *x < d))
            {
                let body = &self.buffer[at..];
                let opener = tool_xml::opener_len(body);
                if quoted(&self.context(&output.text, &self.buffer[..at])) {
                    output.text.push_str(&self.buffer[..at + opener]);
                    self.buffer.drain(..at + opener);
                    continue;
                }
                match tool_xml::parse(
                    body,
                    end,
                    |name| self.xml_name(name),
                    |name, key| string_field(name, key, &self.tools),
                ) {
                    Parsed::Wait => break,
                    Parsed::Text => {
                        output.text.push_str(&self.buffer[..at + opener]);
                        self.buffer.drain(..at + opener);
                    }
                    Parsed::CutShort => {
                        output.text.push_str(&self.buffer[..at]);
                        return Err(transport(CUT_SHORT));
                    }
                    Parsed::Call { name, args, rest } => {
                        let rest = rest.to_owned();
                        output.text.push_str(&self.buffer[..at]);
                        self.append_call(
                            output,
                            ToolCall {
                                id: format!("call_{}", id()),
                                name,
                                arguments: args.to_string(),
                            },
                        )?;
                        self.buffer = rest;
                    }
                }
                continue;
            }
            if let Some(at) = d.filter(|d| open.as_ref().map_or(true, |o| *d < o.0)) {
                if quoted(&self.context(&output.text, &self.buffer[..at])) {
                    let to = at + self.buffer[at..].chars().next().unwrap().len_utf8();
                    output.text.push_str(&self.buffer[..to]);
                    self.buffer.drain(..to);
                    continue;
                }
                output.text.push_str(&self.buffer[..at]);
                self.buffer.drain(..at);
                let Some(length) = dsml_tag(&self.buffer) else {
                    if let Some(next) = self.buffer[1..].find('<').map(|i| i + 1) {
                        self.buffer.drain(..next);
                        continue;
                    }
                    if !end {
                        break;
                    }
                    if self.buffer.contains("invoke") || self.buffer.contains("parameter") {
                        return Err(transport(CUT_SHORT));
                    }
                    self.buffer.clear();
                    break;
                };
                let Some(name) = dsml_invoke(&self.buffer[..length]) else {
                    self.buffer.drain(..length);
                    self.after = true;
                    continue;
                };
                let body = &self.buffer[length..];
                let closing = re(r"<\s*/\s*[|｜][\s|｜]*DSML[\s|｜]*invoke\s*>")
                    .find(body)
                    .map(|m| (m.start(), m.end()));
                let next = re(r#"<\s*[|｜][\s|｜]*DSML[\s|｜]*invoke\s+name\s*="#)
                    .find(body)
                    .map(|m| m.start());
                let stop = closing.map(|c| c.0).into_iter().chain(next).min();
                let Some(stop) = stop else {
                    if !end {
                        break;
                    }
                    return Err(transport(CUT_SHORT));
                };
                let arguments = dsml_args(&body[..stop])?;
                let consumed = closing.filter(|c| c.0 == stop).map(|c| c.1).unwrap_or(stop);
                let rest = body[consumed..].to_owned();
                self.append_call(
                    output,
                    ToolCall {
                        id: format!("call_{}", id()),
                        name,
                        arguments,
                    },
                )?;
                self.buffer = rest;
                continue;
            }
            let Some((at, length, named)) = open else {
                break;
            };
            let head = &self.buffer[..at];
            if quoted(&self.context(&output.text, head)) {
                output.text.push_str(&self.buffer[..at + length]);
                self.buffer.drain(..at + length);
                continue;
            }
            let body = &self.buffer[at + length..];
            let inner = body.trim_start();
            if named.is_none() && (inner.starts_with('<') || end && inner.is_empty()) {
                output.text.push_str(head);
                self.buffer = inner.to_owned();
                self.after = true;
                continue;
            }
            match self.block(body, named.as_deref(), end)? {
                Block::Wait => break,
                Block::Text => {
                    output.text.push_str(&self.buffer[..at + length]);
                    self.buffer.drain(..at + length);
                }
                Block::Call(call, rest) => {
                    output.text.push_str(head);
                    self.append_call(output, call)?;
                    self.buffer = rest;
                }
            }
        }
        if end {
            if let Some(m) = re(r"<tool_?call(?:\s[^<>{}\n]{0,200})?$").find(&self.buffer) {
                if !quoted(&self.context(&output.text, &self.buffer[..m.start()])) {
                    return Err(transport(CUT_SHORT));
                }
            }
            if self.buffer.contains("DSML") && dsml_at(&self.buffer).is_some() {
                return Err(transport(CUT_SHORT));
            }
            output.text.push_str(&self.buffer);
            self.buffer.clear();
        } else {
            let mut keep = 0usize;
            let candidates = [
                opener(&self.buffer).map(|o| o.0),
                re(r"<tool_?call(?:\s[^<>{}\n]{0,200})?$")
                    .find(&self.buffer)
                    .map(|m| m.start()),
                dsml_at(&self.buffer),
                tool_xml::at(&self.buffer),
                tool_xml::hold_at(&self.buffer),
                re(r#"\{\s*"reasoning_content"\s*:"#)
                    .find(&self.buffer)
                    .map(|m| m.start()),
            ];
            if let Some(start) = candidates.into_iter().flatten().min() {
                keep = self.buffer.len() - start;
            }
            if let Some(start) = reason_prefix(&self.buffer) {
                keep = keep.max(self.buffer.len() - start);
            }
            if keep == 0 {
                for (start, _) in self.buffer.char_indices().rev() {
                    let tail = &self.buffer[start..];
                    if tail.len() > 64 {
                        break;
                    }
                    if marks().any(|mark| mark.starts_with(tail)) {
                        keep = tail.len();
                    }
                }
            }
            let split = self.buffer.len() - keep;
            if let Some(m) = re(r#"[}\]"'>][\s}\]"'>]*$"#).find(&self.buffer[..split]) {
                keep = self.buffer.len() - m.start();
            }
            let split = self.buffer.len() - keep;
            output.text.push_str(&self.buffer[..split]);
            self.buffer.drain(..split);
        }
        Ok(())
    }

    fn xml_name(&self, name: &str) -> Option<String> {
        if tool(name, &self.tools).is_some() {
            return Some(name.into());
        }
        let wanted = normalized_tool_name(name);
        self.tools
            .iter()
            .map(function)
            .filter_map(|tool| tool["name"].as_str())
            .find(|name| normalized_tool_name(name) == wanted)
            .map(str::to_owned)
    }
}
#[cfg(test)]
#[path = "../test/tools.test.rs"]
mod tests;
