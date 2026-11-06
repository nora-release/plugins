use crate::*;
use nora_provider::tool_xml::{self, Parsed};
use regex::Regex;
use std::sync::OnceLock;
const OPEN: &str = "<tool_call>";
const CLOSE: &str = "</tool_call>";
const CUT_SHORT: &str = "Qoder's text tool call was cut short; it was not sent as an answer.";
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
    tools: Vec<Value>,
    after: bool,
    failed: bool,
}
fn string_field(name: &str, key: &str, tools: &[Value]) -> bool {
    tools
        .iter()
        .map(|tool| tool.get("function").unwrap_or(tool))
        .find(|tool| tool["name"].as_str() == Some(name))
        .is_some_and(|tool| tool["parameters"]["properties"][key]["type"] == "string")
}
fn parse(body: &str, tools: &[Value]) -> Option<(String, String)> {
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
            tool_xml::value(value, string_field(name, key, tools)),
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
    pub fn new(tools: Vec<Value>) -> Self {
        Self {
            tools: tools
                .into_iter()
                .filter(|tool| {
                    tool.get("function").unwrap_or(tool)["name"]
                        .as_str()
                        .is_some_and(|name| !name.is_empty())
                })
                .collect(),
            ..Self::default()
        }
    }

    fn text(&mut self, output: &mut Vec<Piece>, text: &str) {
        if text.is_empty() {
            return;
        }
        self.sent.push_str(text);
        output.push(Piece::Text(text.into()));
    }
    pub fn feed(&mut self, input: &str) -> Vec<Piece> {
        if self.failed {
            return Vec::new();
        }
        if self.sent.len() + self.text_buf.len() + self.call_buf.len() + input.len()
            > 8 * 1024 * 1024
        {
            self.failed = true;
            return vec![Piece::Error("Qoder's tool-call text exceeds 8 MiB.")];
        }
        let mut output = Vec::new();
        let mut input = input.to_owned();
        while !input.is_empty() {
            if !self.in_call {
                let mut combined = std::mem::take(&mut self.text_buf);
                combined.push_str(&input);
                let Some(combined) = self.left(combined, false) else {
                    break;
                };
                let open = combined.find(OPEN);
                if let Some(at) = self
                    .xml_at(&combined)
                    .filter(|at| open.is_none_or(|open| *at < open))
                {
                    let Some(rest) = self.bare(&mut output, &combined, at, false) else {
                        break;
                    };
                    input = rest;
                    continue;
                }
                let Some(index) = open else {
                    let marks = std::iter::once(OPEN).chain(
                        tool_xml::OPEN_MARKS
                            .iter()
                            .copied()
                            .filter(|_| !self.tools.is_empty()),
                    );
                    let mut keep = marks
                        .flat_map(|mark| {
                            (1..mark.len()).filter_map(|length| {
                                combined.ends_with(&mark[..length]).then_some(length)
                            })
                        })
                        .max()
                        .unwrap_or(0);
                    if !self.tools.is_empty() {
                        if let Some(start) = tool_xml::hold_at(&combined) {
                            keep = keep.max(combined.len() - start);
                        }
                    }
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
            if let Some((name, args)) = parse(body, &self.tools) {
                self.saw_tool = true;
                output.push(Piece::Call { name, args });
            } else if callish(body) && !quoted(&self.sent) {
                self.in_call = false;
                self.failed = true;
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
        if self.failed {
            return Vec::new();
        }
        if self.in_call {
            self.in_call = false;
            let body = std::mem::take(&mut self.call_buf);
            if callish(&body) && !quoted(&self.sent) {
                if let Some((name, args)) = parse(&body, &self.tools) {
                    self.saw_tool = true;
                    return vec![Piece::Call { name, args }];
                }
                self.failed = true;
                return vec![Piece::Error("Qoder's text tool call ended before it was complete; it was not sent as an answer.")];
            }
            let value = format!("{OPEN}{body}");
            self.sent.push_str(&value);
            return vec![Piece::Text(value)];
        }
        let mut output = Vec::new();
        let mut combined = std::mem::take(&mut self.text_buf);
        while !combined.is_empty() {
            let Some(rest) = self.left(combined, true) else {
                break;
            };
            combined = rest;
            let Some(at) = self.xml_at(&combined) else {
                self.text(&mut output, &combined);
                break;
            };
            let Some(rest) = self.bare(&mut output, &combined, at, true) else {
                break;
            };
            combined = rest;
        }
        output
    }

    fn xml_at(&self, text: &str) -> Option<usize> {
        if self.tools.is_empty() {
            return None;
        }
        tool_xml::at(text)
    }

    fn left(&mut self, text: String, end: bool) -> Option<String> {
        if !self.after {
            return Some(text);
        }
        let rest = &text[tool_xml::leftover_len(&text)..];
        if !end
            && (rest.is_empty()
                || tool_xml::CLOSE_MARKS
                    .iter()
                    .any(|mark| mark.starts_with(rest)))
        {
            self.text_buf = rest.into();
            return None;
        }
        self.after = false;
        Some(rest.into())
    }

    fn bare(
        &mut self,
        output: &mut Vec<Piece>,
        text: &str,
        at: usize,
        end: bool,
    ) -> Option<String> {
        let head = &text[..at];
        let body = &text[at..];
        let opener = tool_xml::opener_len(body);
        if quoted(&format!("{}{head}", self.sent)) {
            self.text(output, &text[..at + opener]);
            return Some(body[opener..].into());
        }
        match tool_xml::parse(
            body,
            end,
            |name| {
                self.tools
                    .iter()
                    .any(|tool| tool.get("function").unwrap_or(tool)["name"].as_str() == Some(name))
                    .then(|| name.to_owned())
            },
            |name, key| string_field(name, key, &self.tools),
        ) {
            Parsed::Wait => {
                self.text(output, head);
                self.text_buf = body.into();
                None
            }
            Parsed::Text => {
                self.text(output, &text[..at + opener]);
                Some(body[opener..].into())
            }
            Parsed::CutShort => {
                self.text(output, head);
                output.push(Piece::Error(CUT_SHORT));
                self.failed = true;
                None
            }
            Parsed::Call { name, args, rest } => {
                self.text(output, head);
                output.push(Piece::Call {
                    name,
                    args: args.to_string(),
                });
                self.saw_tool = true;
                self.after = true;
                Some(rest.into())
            }
        }
    }
}
#[cfg(test)]
#[path = "../test/splitter.test.rs"]
mod tests;
