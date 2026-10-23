use crate::*;
use regex::Regex;
use std::sync::OnceLock;
const OPEN: &str = "<tool_call>";
const CLOSE: &str = "</tool_call>";
#[derive(Debug)]
pub(super) enum Piece {
    Text(String),
    Call { name: String, args: String },
    Error(&'static str),
}
#[derive(Default)]
pub(super) struct Splitter {
    text_buf: String,
    call_buf: String,
    in_call: bool,
    pub saw_tool: bool,
    sent: String,
}
fn parse(body: &str) -> Option<(String, String)> {
    if let Ok(value) = serde_json::from_str::<Value>(body.trim()) {
        if !text(&value, "name").trim().is_empty()
            && value.get("arguments").is_some_and(Value::is_object)
        {
            return Some((
                text(&value, "name").trim().into(),
                value["arguments"].to_string(),
            ));
        }
    }
    static FUNCTION: OnceLock<Regex> = OnceLock::new();
    static PARAM: OnceLock<Regex> = OnceLock::new();
    let function = FUNCTION
        .get_or_init(|| Regex::new(r"(?s)<function=([^>]+)>(.*?)</function>").unwrap())
        .captures(body)?;
    let name = function.get(1)?.as_str().trim();
    if name.is_empty() {
        return None;
    }
    let mut args = serde_json::Map::new();
    for parameter in PARAM
        .get_or_init(|| Regex::new(r"(?s)<parameter=([^>]+)>(.*?)</parameter>").unwrap())
        .captures_iter(function.get(2)?.as_str())
    {
        let key = parameter.get(1)?.as_str().trim();
        if key.is_empty() {
            continue;
        }
        let value = parameter.get(2)?.as_str().trim();
        args.insert(
            key.into(),
            serde_json::from_str(value).unwrap_or_else(|_| json!(value)),
        );
    }
    Some((name.into(), Value::Object(args).to_string()))
}
fn callish(value: &str) -> bool {
    value.trim_start().starts_with('{') || value.contains("<function=")
}
fn quoted(value: &str) -> bool {
    if value.matches("```").count() % 2 == 1 {
        return true;
    }
    value
        .rsplit('\n')
        .next()
        .unwrap_or("")
        .replace("```", "")
        .matches('`')
        .count()
        % 2
        == 1
}
impl Splitter {
    fn text(&mut self, output: &mut Vec<Piece>, text: &str) {
        if text.is_empty() {
            return;
        }
        self.sent.push_str(text);
        output.push(Piece::Text(text.into()));
    }
    pub fn feed(&mut self, input: &str) -> Vec<Piece> {
        if self.sent.len() + self.text_buf.len() + self.call_buf.len() + input.len()
            > 8 * 1024 * 1024
        {
            return vec![Piece::Error("Qoder's tool-call text exceeds 8 MiB.")];
        }
        let mut output = Vec::new();
        let mut input = input.to_owned();
        while !input.is_empty() {
            if !self.in_call {
                let mut combined = std::mem::take(&mut self.text_buf);
                combined.push_str(&input);
                let Some(index) = combined.find(OPEN) else {
                    let keep = (1..OPEN.len())
                        .rev()
                        .find(|length| combined.ends_with(&OPEN[..*length]))
                        .unwrap_or(0);
                    self.text(&mut output, &combined[..combined.len() - keep]);
                    self.text_buf = combined[combined.len() - keep..].into();
                    break;
                };
                self.text(&mut output, &combined[..index]);
                input = combined[index + OPEN.len()..].into();
                self.in_call = true;
                continue;
            }
            let mut combined = std::mem::take(&mut self.call_buf);
            combined.push_str(&input);
            let Some(index) = combined.find(CLOSE) else {
                self.call_buf = combined;
                break;
            };
            let body = &combined[..index];
            if let Some((name, args)) = parse(body) {
                self.saw_tool = true;
                output.push(Piece::Call { name, args });
            } else if callish(body) && !quoted(&self.sent) {
                self.in_call = false;
                output.push(Piece::Error("Qoder's text tool call could not be read; the incomplete call was not sent as an answer."));
                return output;
            } else {
                self.text(&mut output, &format!("{OPEN}{body}{CLOSE}"));
            }
            input = combined[index + CLOSE.len()..].into();
            self.in_call = false;
        }
        output
    }
    pub fn flush(&mut self) -> Vec<Piece> {
        if self.in_call {
            self.in_call = false;
            let body = std::mem::take(&mut self.call_buf);
            if callish(&body) && !quoted(&self.sent) {
                if let Some((name, args)) = parse(&body) {
                    self.saw_tool = true;
                    return vec![Piece::Call { name, args }];
                }
                return vec![Piece::Error("Qoder's text tool call ended before it was complete; it was not sent as an answer.")];
            }
            let value = format!("{OPEN}{body}");
            self.sent.push_str(&value);
            return vec![Piece::Text(value)];
        }
        let value = std::mem::take(&mut self.text_buf);
        if value.is_empty() {
            Vec::new()
        } else {
            self.sent.push_str(&value);
            vec![Piece::Text(value)]
        }
    }
}
#[cfg(test)]
#[path = "../test/splitter.test.rs"]
mod tests;
