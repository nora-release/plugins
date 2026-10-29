mod conditions;
mod operations;
mod paths;
use nora_gateway_middleware::{
    value::{array, keys},
    Action, Context, Middleware,
};
use serde_json::{json, Value};
#[derive(Default)]
pub struct Plugin;
impl Middleware for Plugin {
    const NAME: &'static str = "param-override";
    fn apply(&self, phase: &str, body: &mut Value, ctx: &mut Context) -> Result<Action, String> {
        if phase != "request" || !ctx.options.is_object() {
            return Ok(Action::Keep);
        }
        let req = json!({"model":ctx.model,"original_model":ctx.model,"upstream_model":ctx.model,"request_path":ctx.path,"protocol":ctx.protocol,"agent":ctx.agent,"stream":ctx.stream});
        let before = body.clone();
        for key in keys(&ctx.options) {
            if key != "operations" {
                if let Some(o) = body.as_object_mut() {
                    o.insert(key.clone(), ctx.options[&key].clone());
                }
            }
        }
        for op in array(&ctx.options["operations"]) {
            if !op.is_object()
                || !conditions::when(
                    body,
                    &req,
                    &op["conditions"],
                    op["logic"].as_str().unwrap_or("OR"),
                )
            {
                continue;
            }
            if let Some(reject) = operations::run(body, op, &req)? {
                return Ok(reject);
            }
        }
        Ok(nora_gateway_middleware::value::changed(&before, body))
    }
}
