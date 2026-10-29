use super::*;
use serde_json::json;

#[test]
fn project_region_paths_and_explicit_source_are_validated_without_reading_an_account() {
    for (location, host) in [
        ("global", "aiplatform.googleapis.com"),
        ("us", "aiplatform.us.rep.googleapis.com"),
        ("eu", "aiplatform.eu.rep.googleapis.com"),
        (
            "asia-northeast1",
            "asia-northeast1-aiplatform.googleapis.com",
        ),
    ] {
        let config = Config::from_inputs(
            &json!({"project":"project-one","location":location,"sourcePath":"/not/read/adc.json"}),
        )
        .unwrap();
        assert_eq!(config.root(), format!("https://{host}"));
        assert_eq!(
            config.base(),
            format!(
                "https://{host}/v1/projects/project-one/locations/{location}/publishers/google"
            )
        );
    }
    for inputs in [
        json!({"project":"evil/path","sourcePath":"/tmp/adc"}),
        json!({"project":"p-one","location":"us.evil.example","sourcePath":"/tmp/adc"}),
        json!({"project":"p-one","sourcePath":"relative.json"}),
        json!({"project":"p-one","sourcePath":"/tmp/adc","impersonate":"user@example.com/steal"}),
    ] {
        assert!(Config::from_inputs(&inputs).is_err());
    }
    assert!(credentials_file("relative/path").is_err());
    assert!(Config::from_saved(Some(&json!({"project":"p-one","location":"global","sourcePath":"/tmp/adc","impersonate":"","identity":""}))).is_err());
}

#[test]
fn optional_paygo_headers_round_trip_per_account_and_old_accounts_default_to_no_headers() {
    let mut config = Config::from_inputs(&json!({"project":"project-one","sourcePath":"/tmp/adc","header:X-Vertex-AI-LLM-Request-Type":"dedicated","header:X-Vertex-AI-LLM-Shared-Request-Type":"flex"})).unwrap();
    config.identity = "a".repeat(64);
    let saved = serde_json::to_value(&config).unwrap();
    let restored = Config::from_saved(Some(&saved)).unwrap();
    assert_eq!(
        restored.headers["X-Vertex-AI-LLM-Request-Type"],
        "dedicated"
    );
    assert_eq!(
        restored.headers["X-Vertex-AI-LLM-Shared-Request-Type"],
        "flex"
    );
    let mut old = saved;
    old.as_object_mut().unwrap().remove("headers");
    assert!(Config::from_saved(Some(&old)).unwrap().headers.is_empty());
}

#[test]
fn optional_vertex_headers_reject_control_characters_types_and_authentication_overrides() {
    for extra in [
        json!({"header:X-Vertex-AI-LLM-Request-Type":"dedicated\r\nInjected: yes"}),
        json!({"header:X-Vertex-AI-LLM-Request-Type":"\ndedicated"}),
        json!({"header:X-Vertex-AI-LLM-Request-Type":1}),
        json!({"headers":{"Authorization":"Bearer wrong"}}),
        json!({"headers":{"X-Vertex-AI-LLM-Request-Type":"x".repeat(513)}}),
    ] {
        let mut input = json!({"project":"project-one","sourcePath":"/tmp/adc"});
        input
            .as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        assert!(Config::from_inputs(&input).is_err());
    }
}
