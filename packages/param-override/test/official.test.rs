use nora_gateway_middleware::{Context, Middleware};
use serde_json::json;
#[path = "../../middleware-runtime/test/official.rs"]
mod official;

#[test]
fn official_cases() {
    official::run::<nora_provider_param_override::Plugin>(include_str!("fixtures/cases.json"));
}

#[test]
fn full_numeric_conditions_use_javascript_number_equality() {
    let mut body = json!({"level":1.0});
    let mut context = Context {
        options: json!({"operations":[{"path":"hit","mode":"set","value":true,"conditions":[{"path":"level","value":1,"mode":"full"}]}]}),
        ..Default::default()
    };
    nora_provider_param_override::Plugin
        .apply("request", &mut body, &mut context)
        .unwrap();
    assert_eq!(body["hit"], true);
}
