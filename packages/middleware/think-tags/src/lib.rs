mod strip;
use nora_gateway_middleware::{
    value::{changed, string, truthy},
    Action, Context, Middleware,
};
use serde_json::{json, Value};
#[derive(Default)]
pub struct Plugin;
impl Middleware for Plugin {
    const NAME: &'static str = "think-tags";
    fn apply(&self, phase: &str, body: &mut Value, ctx: &mut Context) -> Result<Action, String> {
        if !matches!(phase, "event" | "response") || (phase == "response" && ctx.status >= 400) {
            return Ok(Action::Keep);
        }
        let before = body.clone();
        if ctx.options["mode"] == "to_content" {
            to_content(body, &mut ctx.state, phase == "event");
        } else if phase == "event" {
            event(body, &mut ctx.state);
        } else {
            response(body);
        }
        Ok(changed(&before, body))
    }
}
fn key(v: &Value, name: &str) -> String {
    v.get(name)
        .map(string)
        .unwrap_or_else(|| "undefined".into())
}
fn edit(v: &mut Value, pointer: &str) {
    if let Some(v) = v.pointer_mut(pointer) {
        if let Some(t) = v.as_str() {
            *v = json!(strip::whole(t));
        }
    }
}
fn stream(v: &mut Value, pointer: &str, state: &mut Value, key: &str) {
    if let Some(v) = v.pointer_mut(pointer) {
        if let Some(t) = v.as_str() {
            *v = json!(strip::piece(state, key, t));
        }
    }
}
fn item(v: &mut Value) {
    if v["type"] != "message" {
        return;
    }
    if let Some(parts) = v["content"].as_array_mut() {
        if let Some(part) = parts.iter_mut().find(|p| p["type"] == "output_text") {
            edit(part, "/text");
        }
    }
}
fn response(v: &mut Value) {
    if let Some(parts) = v.get_mut("content").and_then(Value::as_array_mut) {
        if let Some(part) = parts.iter_mut().find(|p| p["type"] == "text") {
            edit(part, "/text");
        }
    }
    if let Some(choices) = v.get_mut("choices").and_then(Value::as_array_mut) {
        for c in choices {
            edit(c, "/message/content");
        }
    }
    if let Some(items) = v.get_mut("output").and_then(Value::as_array_mut) {
        for it in items {
            item(it);
        }
    }
}
fn event(v: &mut Value, state: &mut Value) {
    match v["type"].as_str() {
        Some("content_block_delta") if v["delta"]["type"] == "text_delta" => {
            let k = format!("a{}", key(v, "index"));
            stream(v, "/delta/text", state, &k);
            return;
        }
        Some("response.output_text.delta") => {
            let k = format!("r{}:{}", key(v, "item_id"), key(v, "content_index"));
            stream(v, "/delta", state, &k);
            return;
        }
        Some("response.output_text.done") if v["text"].is_string() => {
            edit(v, "/text");
            return;
        }
        Some("response.content_part.done") if v["part"]["text"].is_string() => {
            edit(v, "/part/text");
            return;
        }
        Some("response.output_item.done") if truthy(&v["item"]) => {
            item(&mut v["item"]);
            return;
        }
        Some("response.completed") if v["response"]["output"].is_array() => {
            for it in v["response"]["output"].as_array_mut().unwrap() {
                item(it);
            }
            return;
        }
        _ => {}
    }
    if let Some(choices) = v.get_mut("choices").and_then(Value::as_array_mut) {
        for c in choices {
            if !c["delta"].is_object() {
                continue;
            }
            let k = format!("c{}", key(c, "index"));
            stream(c, "/delta/content", state, &k);
            if truthy(&c["finish_reason"]) && !truthy(&state[&k]["inside"]) {
                if let Some(carry) = state[&k]["carry"].as_str().filter(|s| !s.is_empty()) {
                    c["delta"]["content"] = json!(format!(
                        "{}{carry}",
                        c["delta"]["content"].as_str().unwrap_or("")
                    ));
                    state[&k]["carry"] = json!("");
                }
            }
        }
    }
}
fn reasoning(m: &Value) -> Option<&str> {
    m["reasoning_content"]
        .as_str()
        .or_else(|| m["reasoning"].as_str())
}
fn to_content(v: &mut Value, state: &mut Value, event: bool) {
    let Some(choices) = v.get_mut("choices").and_then(Value::as_array_mut) else {
        return;
    };
    for c in choices {
        let k = format!("t{}", key(c, "index"));
        let finish = truthy(&c["finish_reason"]);
        let Some(m) = c
            .get_mut(if event { "delta" } else { "message" })
            .filter(|m| m.is_object())
        else {
            continue;
        };
        let r = reasoning(m).map(str::to_owned);
        let content = m["content"].as_str().unwrap_or("").to_string();
        if !event {
            if let Some(r) = r.filter(|s| !s.is_empty()) {
                let o = m.as_object_mut().unwrap();
                o.shift_remove("reasoning_content");
                o.shift_remove("reasoning");
                o.insert(
                    "content".into(),
                    json!(format!("<think>\n{r}\n</think>\n\n{content}")),
                );
            }
            continue;
        }
        let mut text = String::new();
        if let Some(r) = r {
            let o = m.as_object_mut().unwrap();
            o.shift_remove("reasoning_content");
            o.shift_remove("reasoning");
            if !r.is_empty() {
                if !truthy(&state[&k]) {
                    state[&k] = json!("open");
                    text.push_str("<think>\n");
                }
                text.push_str(&r);
            }
        }
        if state[&k] == "open" && (!content.is_empty() || finish) {
            text.push_str("\n</think>\n\n");
            state[&k] = json!("closed");
        }
        if !text.is_empty() {
            m["content"] = json!(text + &content);
        }
    }
}
