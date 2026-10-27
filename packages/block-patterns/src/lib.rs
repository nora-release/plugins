use nora_gateway_middleware::{
    regex::matcher,
    value::{array, truthy},
    Action, Context, Middleware,
};
use serde_json::Value;
#[derive(Default)]
pub struct Plugin;
impl Middleware for Plugin {
    const NAME: &'static str = "block-patterns";
    fn apply(&self, phase: &str, body: &mut Value, ctx: &mut Context) -> Result<Action, String> {
        if phase != "request" {
            return Ok(Action::Keep);
        }
        let Some(re) = matcher(&ctx.options, true)? else {
            return Ok(Action::Keep);
        };
        let mut hit = None;
        scan(
            body,
            &ctx.protocol,
            ctx.options["system"] != false,
            &mut |text| {
                if hit.is_none() {
                    hit = re.first(text).map(str::to_owned);
                }
            },
        );
        let Some(hit) = hit else {
            return Ok(Action::Keep);
        };
        let message = ctx.options["message"]
            .as_str()
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .unwrap_or_else(|| {
                format!("this request has \"{hit}\", which this gateway doesn't send")
            });
        Ok(Action::Reject {
            status: 400,
            message,
        })
    }
}
fn texts(value: &Value, f: &mut impl FnMut(&str)) {
    if let Some(text) = value.as_str() {
        f(text);
        return;
    }
    for p in array(value) {
        if matches!(
            p["type"].as_str(),
            Some("text" | "input_text" | "output_text")
        ) {
            if let Some(text) = p["text"].as_str() {
                f(text);
            }
        }
    }
}
fn parts(value: &Value, f: &mut impl FnMut(&str)) {
    for p in array(value) {
        if !truthy(&p["thought"]) {
            if let Some(text) = p["text"].as_str() {
                f(text);
            }
        }
    }
}
fn scan(body: &Value, protocol: &str, system: bool, f: &mut impl FnMut(&str)) {
    if protocol == "gemini" {
        for m in array(&body["contents"]) {
            if m.is_object() && (m.get("role").is_none() || m["role"] == "user") {
                parts(&m["parts"], f);
            }
        }
        if system {
            for k in ["systemInstruction", "system_instruction"] {
                if let Some(text) = body[k].as_str() {
                    f(text);
                } else {
                    parts(&body[k]["parts"], f);
                }
            }
        }
        return;
    }
    if protocol == "responses" {
        if system {
            if let Some(t) = body["instructions"].as_str() {
                f(t);
            }
            for m in array(&body["messages"]) {
                if matches!(m["role"].as_str(), Some("system" | "developer")) {
                    texts(&m["content"], f);
                }
            }
        }
        if let Some(t) = body["input"].as_str() {
            f(t);
            return;
        }
        for m in array(&body["input"]) {
            if !m.is_object() {
                continue;
            }
            if matches!(m["role"].as_str(), Some("system" | "developer")) {
                if system {
                    texts(&m["content"], f);
                }
            } else if m.get("role").is_none() || m["role"] == "user" {
                texts(&m["content"], f);
            }
        }
        return;
    }
    if system && protocol == "anthropic" {
        texts(&body["system"], f);
    }
    for m in array(&body["messages"]) {
        if m["role"] == "user"
            || (system && matches!(m["role"].as_str(), Some("system" | "developer")))
        {
            texts(&m["content"], f);
        }
    }
}
