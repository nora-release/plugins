use nora_gateway_middleware::{
    regex::matcher,
    text,
    value::{array, changed, string, truthy},
    Action, Context, Middleware,
};
use serde_json::{json, Value};
#[derive(Default)]
pub struct Plugin;
impl Middleware for Plugin {
    const NAME: &'static str = "word-guard";
    fn apply(&self, phase: &str, body: &mut Value, ctx: &mut Context) -> Result<Action, String> {
        let o = &ctx.options;
        let masking = o["action"] == "mask";
        if phase != "request"
            && (!matches!(phase, "event" | "response")
                || !truthy(&o["replies"])
                || !masking
                || (phase == "response" && ctx.status >= 400))
        {
            return Ok(Action::Keep);
        }
        let Some(re) = matcher(o, false)? else {
            return Ok(Action::Keep);
        };
        let before = body.clone();
        let mask = o["mask"].as_str().unwrap_or("***");
        if phase == "request" {
            let mut hit = None;
            users(
                body,
                &ctx.protocol,
                o["check"] == "all",
                &mut |text, missing| {
                    if masking {
                        if missing {
                            return Err("User text is not a string".into());
                        }
                        let s = text.as_str().ok_or("User text is not a string")?;
                        *text = json!(re.replace(s, mask, true));
                    } else if hit.is_none() {
                        hit = re
                            .first(&if missing {
                                "undefined".into()
                            } else {
                                string(text)
                            })
                            .map(str::to_owned);
                    }
                    Ok(())
                },
            )?;
            if let Some(hit) = hit {
                let message = if truthy(&o["message"]) {
                    o["message"]
                        .as_str()
                        .unwrap_or("turned away by a gateway middleware")
                        .into()
                } else {
                    format!("this request has \"{hit}\", which this gateway doesn't send")
                };
                return Ok(Action::Reject {
                    status: 400,
                    message,
                });
            }
        } else {
            let mut sub = |s: &str| re.replace(s, mask, true);
            if phase == "event" {
                event(body, &mut sub);
            } else {
                response(body, &mut sub);
            }
        }
        Ok(changed(&before, body))
    }
}
fn users(
    body: &mut Value,
    protocol: &str,
    all: bool,
    f: &mut impl FnMut(&mut Value, bool) -> Result<(), String>,
) -> Result<(), String> {
    if protocol == "responses" && body["input"].is_string() {
        return f(&mut body["input"], false);
    }
    let field = match protocol {
        "gemini" => "contents",
        "responses" => "input",
        _ => "messages",
    };
    let Some(messages) = body.get_mut(field).and_then(Value::as_array_mut) else {
        return Ok(());
    };
    let part_ok = |p: &Value| match protocol {
        "gemini" => p["text"].is_string(),
        "responses" => matches!(p["type"].as_str(), Some("input_text" | "text")),
        _ => p["type"] == "text",
    };
    let mut selected: Vec<_> = messages
        .iter()
        .enumerate()
        .filter(|(_, m)| m["role"] == "user" || (protocol == "gemini" && !truthy(&m["role"])))
        .filter(|(_, m)| {
            m["content"].is_string()
                || array(
                    &m[if protocol == "gemini" {
                        "parts"
                    } else {
                        "content"
                    }],
                )
                .iter()
                .any(&part_ok)
        })
        .map(|(i, _)| i)
        .collect();
    if !all && selected.len() > 1 {
        selected.drain(..selected.len() - 1);
    }
    for i in selected {
        let m = &mut messages[i];
        if m["content"].is_string() {
            f(&mut m["content"], false)?;
        } else if let Some(parts) = m
            .get_mut(if protocol == "gemini" {
                "parts"
            } else {
                "content"
            })
            .and_then(Value::as_array_mut)
        {
            for part in parts {
                if part_ok(part) {
                    if let Some(t) = part.get_mut("text") {
                        f(t, false)?;
                    } else {
                        let mut missing = json!("undefined");
                        f(&mut missing, true)?;
                    }
                }
            }
        }
    }
    Ok(())
}
fn at(body: &mut Value, pointer: &str, f: &mut impl FnMut(&str) -> String) {
    if let Some(text) = body.pointer_mut(pointer) {
        text::mutate(text, f);
    }
}
fn candidates(body: &mut Value, f: &mut impl FnMut(&str) -> String) {
    if let Some(cs) = body.get_mut("candidates").and_then(Value::as_array_mut) {
        for c in cs {
            if let Some(parts) = c.pointer_mut("/content/parts") {
                text::parts(parts, f);
            }
        }
    }
}
fn event(body: &mut Value, f: &mut impl FnMut(&str) -> String) {
    match body["type"].as_str() {
        Some("content_block_delta") => at(body, "/delta/text", f),
        Some("response.output_text.delta") => at(body, "/delta", f),
        Some("response.output_text.done") => at(body, "/text", f),
        _ => {
            if let Some(choices) = body.get_mut("choices").and_then(Value::as_array_mut) {
                for c in choices {
                    at(c, "/delta/content", f);
                }
            } else {
                candidates(body, f);
            }
        }
    }
}
fn response(body: &mut Value, f: &mut impl FnMut(&str) -> String) {
    if let Some(parts) = body.get_mut("content").and_then(Value::as_array_mut) {
        for p in parts {
            if p["type"] == "text" {
                at(p, "/text", f);
            }
        }
    }
    if let Some(cs) = body.get_mut("choices").and_then(Value::as_array_mut) {
        for c in cs {
            at(c, "/message/content", f);
        }
    }
    if let Some(items) = body.get_mut("output").and_then(Value::as_array_mut) {
        for item in items {
            if let Some(parts) = item.get_mut("content") {
                text::parts(parts, f);
            }
        }
    }
    candidates(body, f);
}
