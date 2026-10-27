use nora_gateway_middleware::{Context, Middleware};
use serde_json::json;
#[path = "../../middleware-runtime/test/official.rs"]
mod official;

#[test]
fn official_cases() {
    official::run::<nora_provider_model_map::Plugin>(include_str!("fixtures/cases.json"));
}
#[test]
fn numeric_array_mappings_and_sticky_patterns_match_object_keys_and_last_index() {
    for (options, asked, expected) in [
        (json!({"mapping":["one","two"]}), "0", "one"),
        (json!({"mapping":{"/a/y":"b"}}), "a", "a"),
        (json!({"mapping":{"/a/y":"b"}}), "aa", "ab"),
    ] {
        let mut context = Context {
            options,
            state: json!({}),
            ..Default::default()
        };
        let mut body = json!({"model":asked});
        nora_provider_model_map::Plugin
            .apply("request", &mut body, &mut context)
            .unwrap();
        assert_eq!(body["model"], expected);
    }
}
