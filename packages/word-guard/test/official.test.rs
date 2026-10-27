use nora_gateway_middleware::{Action, Context, Middleware};
use serde_json::json;
#[path = "../../middleware-runtime/test/official.rs"]
mod official;

#[test]
fn official_cases() {
    official::run::<nora_provider_word_guard::Plugin>(include_str!("fixtures/cases.json"));
}
#[test]
fn malformed_text_preserves_javascript_reject_and_fail_open_semantics() {
    let mut context = Context {
        protocol: "anthropic".into(),
        options: json!({"words":["undefined"],"message":true}),
        state: json!({}),
        ..Default::default()
    };
    let original = json!({"messages":[{"role":"user","content":[{"type":"text"}]}]});
    let mut body = original.clone();
    assert_eq!(
        nora_provider_word_guard::Plugin
            .apply("request", &mut body, &mut context)
            .unwrap(),
        Action::Reject {
            status: 400,
            message: "turned away by a gateway middleware".into()
        }
    );
    context.options["action"] = json!("mask");
    let mut body = original;
    nora_provider_word_guard::Plugin
        .apply("request", &mut body, &mut context)
        .unwrap_err();
}
