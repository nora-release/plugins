use nora_gateway_middleware::{
    value::{array, changed, string, truthy},
    Action, Context, Middleware,
};
use serde_json::{json, Value};
#[derive(Default)]
pub struct Plugin;
impl Middleware for Plugin {
    const NAME: &'static str = "system-prompt";
    fn apply(&self, phase: &str, body: &mut Value, ctx: &mut Context) -> Result<Action, String> {
        if phase != "request" {
            return Ok(Action::Keep);
        }
        let o = &ctx.options;
        let prompt = o["system_prompt"].as_str().unwrap_or("");
        if prompt.is_empty() {
            return Ok(Action::Keep);
        }
        let agents = array(&o["agents"]);
        if !agents.is_empty() && !agents.contains(&json!(ctx.agent)) {
            return Ok(Action::Keep);
        }
        let models = array(&o["models"]);
        let model = body
            .get("model")
            .filter(|v| !v.is_null())
            .map(string)
            .unwrap_or_else(|| ctx.model.clone());
        if !models.is_empty()
            && !models
                .iter()
                .filter_map(Value::as_str)
                .any(|prefix| model.starts_with(prefix))
        {
            return Ok(Action::Keep);
        }
        let before = body.clone();
        let prepend = o["position"] == "prepend";
        let over = truthy(&o["override"]);
        match ctx.protocol.as_str() {
            "anthropic" => {
                let s = &body["system"];
                if s.is_null() || s == "" || s.as_array().is_some_and(Vec::is_empty) {
                    body["system"] = json!(prompt);
                } else if over {
                    add(&mut body["system"], prompt, prepend);
                }
            }
            "chat" => {
                if let Some(messages) = body["messages"].as_array_mut() {
                    if !messages
                        .first()
                        .is_some_and(|m| matches!(m["role"].as_str(), Some("system" | "developer")))
                    {
                        messages.insert(0, json!({"role":"system","content":prompt}));
                    } else if over {
                        if let Some(c) = messages[0].get_mut("content") {
                            add(c, prompt, prepend);
                        }
                    }
                }
            }
            "responses" => {
                if body["instructions"].as_str().is_none_or(str::is_empty) {
                    body["instructions"] = json!(prompt);
                } else if over {
                    add(&mut body["instructions"], prompt, prepend);
                }
            }
            "gemini" => {
                let k =
                    if truthy(&body["system_instruction"]) && !truthy(&body["systemInstruction"]) {
                        "system_instruction"
                    } else {
                        "systemInstruction"
                    };
                if body[k]["parts"].as_array().is_none_or(Vec::is_empty) {
                    body[k] = json!({"parts":[{"text":prompt}]});
                } else if over {
                    let parts = body[k]["parts"].as_array_mut().unwrap();
                    if prepend {
                        parts.insert(0, json!({"text":prompt}));
                    } else {
                        parts.push(json!({"text":prompt}));
                    }
                }
            }
            _ => {}
        }
        Ok(changed(&before, body))
    }
}
fn add(value: &mut Value, prompt: &str, prepend: bool) {
    if let Some(s) = value.as_str() {
        *value = json!(if prepend {
            format!("{prompt}\n{s}")
        } else {
            format!("{s}\n{prompt}")
        });
    } else if let Some(parts) = value.as_array_mut() {
        let part = json!({"type":"text","text":prompt});
        if prepend {
            parts.insert(0, part);
        } else {
            parts.push(part);
        }
    }
}
