use nora_gateway_middleware::{
    regex::Expression,
    value::{changed, keys, truthy},
    Action, Context, Middleware,
};
use serde_json::{json, Value};
#[derive(Default)]
pub struct Plugin;
impl Middleware for Plugin {
    const NAME: &'static str = "model-map";
    fn apply(&self, phase: &str, body: &mut Value, ctx: &mut Context) -> Result<Action, String> {
        let before = body.clone();
        if phase == "request" {
            let Some(name) = body["model"].as_str() else {
                return Ok(Action::Keep);
            };
            let to = map(name, &ctx.options)?;
            if to == name {
                return Ok(Action::Keep);
            }
            ctx.state["asked"] = json!(name);
            body["model"] = json!(to);
            return Ok(Action::Replace);
        }
        let Some(asked) = ctx.state["asked"].as_str().filter(|s| !s.is_empty()) else {
            return Ok(Action::Keep);
        };
        if truthy(&ctx.options["keep_upstream_name"]) || (phase == "response" && ctx.status >= 400)
        {
            return Ok(Action::Keep);
        }
        if !matches!(phase, "event" | "response") {
            return Ok(Action::Keep);
        }
        if phase == "event"
            && body["type"] == "message_start"
            && body["message"]["model"].is_string()
        {
            body["message"]["model"] = json!(asked);
        } else if body["model"].is_string() && (phase == "response" || body["model"] != asked) {
            body["model"] = json!(asked);
        } else if body["response"]["model"].is_string() {
            body["response"]["model"] = json!(asked);
        } else if body["modelVersion"].is_string() {
            body["modelVersion"] = json!(asked);
        }
        Ok(changed(&before, body))
    }
}
fn map(name: &str, options: &Value) -> Result<String, String> {
    let rules = options
        .get("mapping")
        .filter(|v| v.is_object() || v.is_array())
        .unwrap_or(options);
    let mut exact = std::collections::BTreeMap::new();
    let mut patterns = vec![];
    for key in keys(rules) {
        let Some(to) = rules
            .get(&key)
            .or_else(|| {
                rules
                    .as_array()
                    .and_then(|items| key.parse::<usize>().ok().and_then(|i| items.get(i)))
            })
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
        else {
            continue;
        };
        if key == "keep_upstream_name" {
            continue;
        }
        let pattern = key
            .strip_prefix('/')
            .and_then(|k| k.rsplit_once('/'))
            .filter(|(p, flags)| {
                !p.is_empty()
                    && !p.contains(['\n', '\r', '\u{2028}', '\u{2029}'])
                    && flags.chars().all(|c| c.is_ascii_lowercase())
            });
        if let Some((pattern, flags)) = pattern {
            patterns.push((Expression::new(pattern, &flags.replacen('g', "", 1))?, to));
        } else {
            exact.insert(key, to);
        }
    }
    let mut current = name.to_owned();
    let mut seen = vec![current.clone()];
    for _ in 0..16 {
        let next = exact.get(&current).map(|s| s.to_string()).or_else(|| {
            patterns
                .iter_mut()
                .find_map(|(re, to)| re.tested_replace(&current, to))
        });
        let Some(next) = next else { break };
        if next == current || seen.contains(&next) {
            break;
        }
        seen.push(next.clone());
        current = next;
    }
    Ok(current)
}
