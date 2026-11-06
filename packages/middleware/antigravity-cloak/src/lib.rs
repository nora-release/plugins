use nora_gateway_middleware::{
    regex::{escape, Expression},
    text,
    value::{array, changed, truthy},
    Action, Context, Middleware,
};
use serde_json::Value;
#[derive(Default)]
pub struct Plugin;
impl Middleware for Plugin {
    const NAME: &'static str = "antigravity-cloak";
    fn apply(&self, phase: &str, body: &mut Value, ctx: &mut Context) -> Result<Action, String> {
        if phase != "provider" || truthy(&ctx.options["off"]) {
            return Ok(Action::Keep);
        }
        let accounts = array(&ctx.options["accounts"]);
        if !(if accounts.is_empty() {
            ctx.provider["account"] == "antigravity"
        } else {
            accounts.contains(&ctx.provider["account"])
        }) {
            return Ok(Action::Keep);
        }
        let mut words = vec!["x-anthropic-billing-header", "Claude Agent SDK"];
        words.extend(
            array(&ctx.options["extraWords"])
                .iter()
                .filter_map(Value::as_str)
                .filter(|w| w.encode_utf16().count() > 1),
        );
        let expressions = words
            .into_iter()
            .map(|w| Expression::new(&escape(w), "gi"))
            .collect::<Result<Vec<_>, _>>()?;
        let before = body.clone();
        let mut cloak = |s: &str| {
            let mut s = s.to_owned();
            for re in &expressions {
                s = re.replace_with(&s, true, |m| {
                    let matched = m.as_str(&s);
                    let first = matched.chars().next().unwrap();
                    format!("{first}\u{200b}{}", &matched[first.len_utf8()..])
                });
            }
            s
        };
        match ctx.protocol.as_str() {
            "anthropic" => {
                if let Some(s) = body.get_mut("system") {
                    text::content(s, &mut cloak);
                }
            }
            "chat" => {
                if let Some(ms) = body["messages"].as_array_mut() {
                    for m in ms {
                        text::system_message(m, &mut cloak);
                    }
                }
            }
            "responses" => {
                if let Some(s) = body.get_mut("instructions") {
                    text::mutate(s, &mut cloak);
                }
                if let Some(ms) = body["input"].as_array_mut() {
                    for m in ms {
                        text::system_message(m, &mut cloak);
                    }
                }
            }
            "gemini" => {
                for k in ["systemInstruction", "system_instruction"] {
                    if let Some(parts) = body.get_mut(k).and_then(|v| v.get_mut("parts")) {
                        text::parts(parts, &mut cloak);
                    }
                }
            }
            _ => {}
        }
        Ok(changed(&before, body))
    }
}
