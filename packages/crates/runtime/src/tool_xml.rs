//! Anthropic/Qwen text tool calls, adapted from Magpie's Trae/Qoder plugins (MIT).
//! Name acceptance stays with the provider; this module only reads the wire format.
use regex::Regex;
use serde_json::{Map, Value};
use std::sync::OnceLock;

const STARTS: &[&str] = &["<invoke ", "<antml:invoke ", "<function="];
pub const OPEN_MARKS: &[&str] = &[
    "<function_calls>",
    "<antml:function_calls>",
    "<invoke ",
    "<antml:invoke ",
    "<function=",
];
pub const CLOSE_MARKS: &[&str] = &[
    "</function_calls>",
    "</antml:function_calls>",
    "</invoke>",
    "</antml:invoke>",
    "</function>",
];

struct Patterns {
    at: Regex,
    hold: Regex,
    wrapper: Regex,
    invoke: Regex,
    function: Regex,
    invoke_close: Regex,
    function_close: Regex,
    next: Regex,
    parameter: Regex,
    parameter_close: Regex,
    parameter_start: Regex,
    leftovers: Regex,
    opener: Regex,
}

fn patterns() -> &'static Patterns {
    static PATTERNS: OnceLock<Patterns> = OnceLock::new();
    PATTERNS.get_or_init(|| {
        let re = |s| Regex::new(s).expect("static tool XML pattern");
        Patterns {
            at: re(r"<(?:antml:)?(?:function_calls>|invoke\s+name\s*=|function=)"),
            hold: re(r"<(?:antml:)?(?:invoke\s[^<>]{0,300}|function=[^<>]{0,300})$"),
            wrapper: re(r"^<(?:antml:)?function_calls>"),
            invoke: re(r#"^<(?:antml:)?invoke\s+name\s*=\s*"([^"<>]+)"\s*>"#),
            function: re(r"^<function=([^\s<>]+?)\s*>"),
            invoke_close: re(r"</(?:antml:)?invoke\s*>"),
            function_close: re(r"</function\s*>"),
            next: re(r"<(?:antml:)?invoke\s+name\s*=|<function="),
            parameter: re(r#"<(?:antml:)?parameter(?:\s+name\s*=\s*"([^"]+)"|=([^\s>]+))\s*>"#),
            parameter_close: re(r"</(?:antml:)?parameter>"),
            parameter_start: re(r"<(?:antml:)?parameter\b"),
            leftovers: re(r"^(?:\s+|</(?:antml:)?(?:function_calls|invoke|function)>)+"),
            opener: re(r"^<[^<>]*>?"),
        }
    })
}

pub fn at(text: &str) -> Option<usize> {
    patterns().at.find(text).map(|m| m.start())
}

pub fn hold_at(text: &str) -> Option<usize> {
    patterns().hold.find(text).map(|m| m.start())
}

pub fn opener_len(text: &str) -> usize {
    patterns().opener.find(text).map_or(0, |m| m.end())
}

pub fn leftover_len(text: &str) -> usize {
    patterns().leftovers.find(text).map_or(0, |m| m.end())
}

pub fn value(raw: &str, is_string: bool) -> Value {
    if is_string {
        return Value::String(raw.into());
    }
    serde_json::from_str(raw.trim()).unwrap_or_else(|_| Value::String(raw.into()))
}

#[derive(Debug, PartialEq)]
pub enum Parsed<'a> {
    Wait,
    Text,
    CutShort,
    Call {
        name: String,
        args: Value,
        rest: &'a str,
    },
}

fn parameters(body: &str, name: &str, is_string: &impl Fn(&str, &str) -> bool) -> (Value, bool) {
    let matches: Vec<_> = patterns().parameter.captures_iter(body).collect();
    let mut closed = !matches.is_empty();
    let mut args = Map::new();
    for (index, capture) in matches.iter().enumerate() {
        let key = capture.get(1).or_else(|| capture.get(2)).unwrap().as_str();
        let end = matches
            .get(index + 1)
            .map_or(body.len(), |next| next.get(0).unwrap().start());
        let mut raw = &body[capture.get(0).unwrap().end()..end];
        if let Some(close) = patterns().parameter_close.find(raw) {
            raw = &raw[..close.start()];
        } else {
            closed = false;
        }
        raw = raw
            .strip_prefix("\r\n")
            .or_else(|| raw.strip_prefix('\n'))
            .unwrap_or(raw);
        raw = raw
            .strip_suffix("\r\n")
            .or_else(|| raw.strip_suffix('\n'))
            .unwrap_or(raw);
        args.insert(key.into(), value(raw, is_string(name, key)));
    }
    (Value::Object(args), closed)
}

pub fn parse<'a>(
    mut text: &'a str,
    end: bool,
    resolve_name: impl Fn(&str) -> Option<String>,
    is_string: impl Fn(&str, &str) -> bool,
) -> Parsed<'a> {
    let p = patterns();
    if let Some(wrapper) = p.wrapper.find(text) {
        text = text[wrapper.end()..].trim_start();
        if !end && (text.is_empty() || STARTS.iter().any(|mark| mark.starts_with(text))) {
            return Parsed::Wait;
        }
    }
    let (opening, closing) = match p.invoke.captures(text) {
        Some(opening) => (Some(opening), &p.invoke_close),
        None => (p.function.captures(text), &p.function_close),
    };
    let Some(opening) = opening else {
        if !end && (STARTS.iter().any(|mark| mark.starts_with(text)) || hold_at(text) == Some(0)) {
            return Parsed::Wait;
        }
        return Parsed::Text;
    };
    let Some(name) = resolve_name(opening.get(1).unwrap().as_str().trim()) else {
        return Parsed::Text;
    };
    let body = &text[opening.get(0).unwrap().end()..];
    let close = closing.find(body);
    let stop = close
        .as_ref()
        .map(|m| m.start())
        .into_iter()
        .chain(p.next.find(body).map(|m| m.start()))
        .min();
    if stop.is_none() && !end {
        return Parsed::Wait;
    }
    let stop = stop.unwrap_or(body.len());
    let close = close.filter(|m| m.start() == stop);
    let region = &body[..stop];
    let (args, closed) = parameters(region, &name, &is_string);
    if close.is_none() && !closed {
        if !p.parameter_start.is_match(region) {
            return Parsed::Text;
        }
        return Parsed::CutShort;
    }
    Parsed::Call {
        name,
        args,
        rest: &body[close.map_or(stop, |m| m.end())..],
    }
}

#[cfg(test)]
#[path = "../test/tool_xml.test.rs"]
mod tests;
