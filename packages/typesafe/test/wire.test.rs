use super::*;

fn question(model: &str) -> Value {
    json!({"model":model,"state":{"text":"hello","previous_intent":"coding"},"questions":{"intent":{"type":"choice","criteria":{"coding":"Code","other":"Other"}},"level":{"type":"score","criteria":["low","high"]},"urgent":{"type":"noul"}},"providerOptions":{"gateway":{"zeroDataRetention":true}}})
}

#[test]
fn vercel_typesafe_keeps_the_actual_native_body_and_extensions_unchanged() {
    let original = serde_json::to_vec_pretty(&question("typesafe-ai/jev")).unwrap();
    let (target, bytes, headers) = ask(
        DecisionFamily::Vercel,
        "https://ai-gateway.vercel.sh/typesafe/v1",
        original.clone(),
    )
    .unwrap();
    assert_eq!(target, "https://ai-gateway.vercel.sh/typesafe/v1/systemone");
    assert_eq!(bytes, original);
    assert!(headers.is_empty());
}

#[test]
fn cloudflare_jev_wraps_questions_in_input_without_an_inner_model() {
    let original = question(CF_JEV);
    let (target, bytes, _) = ask(
        DecisionFamily::Cloudflare,
        "https://api.cloudflare.com/client/v4/accounts/acc/ai/run",
        serde_json::to_vec(&original).unwrap(),
    )
    .unwrap();
    let value: Value = serde_json::from_slice(&bytes).unwrap();
    assert!(target.ends_with("/accounts/acc/ai/run"));
    assert_eq!(value["model"], CF_JEV);
    assert!(value["input"].get("model").is_none());
    assert_eq!(value["input"]["state"], original["state"]);
    assert_eq!(value["input"]["questions"], original["questions"]);
}

#[test]
fn clef_uses_its_own_run_path_and_short_model_and_preserves_embedded_media() {
    for (model, short) in [(CF_CLEF, "clef"), (CF_FLASH, "clef-flash")] {
        let mut original = question(model);
        original["images"] = json!(["data:image/png;base64,actual-image"]);
        original["state"] = json!([{"content":"actual state"}]);
        let (target, bytes, _) = ask(
            DecisionFamily::Cloudflare,
            "https://api.cloudflare.com/client/v4/accounts/acc/ai/run",
            serde_json::to_vec(&original).unwrap(),
        )
        .unwrap();
        let value: Value = serde_json::from_slice(&bytes).unwrap();
        assert!(target.ends_with(&format!("/ai/run/{model}")));
        assert_eq!(value["model"], short);
        assert_eq!(value["images"], original["images"]);
        assert_eq!(value["state"], original["state"]);
        assert!(value.get("input").is_none());
    }
}

#[test]
fn workers_ai_rejects_unknown_invocation_shapes_and_explicit_clef_question_overflow() {
    assert!(ask(
        DecisionFamily::Cloudflare,
        "https://api.cloudflare.com/client/v4/accounts/acc/ai/run",
        serde_json::to_vec(&question("a-chat-model")).unwrap()
    )
    .is_err());
    let mut original = question(CF_CLEF);
    original["questions"] = Value::Object(
        (0..65)
            .map(|id| (format!("q{id}"), json!({"type":"noul"})))
            .collect(),
    );
    assert!(ask(
        DecisionFamily::Cloudflare,
        "https://api.cloudflare.com/client/v4/accounts/acc/ai/run",
        serde_json::to_vec(&original).unwrap()
    )
    .unwrap_err()
    .message
    .contains("64"));
}

#[test]
fn cloudflare_envelopes_keep_real_answers_probabilities_and_usage() {
    let result = json!({"model":"clef","answers":{"q":{"choice":"code","probabilities":{"code":0.9,"other":0.1},"confidence":0.8}},"usage":{"input_tokens":31,"output_tokens":0}});
    let bytes = answer(
        DecisionFamily::Cloudflare,
        "https://api.cloudflare.com/client/v4",
        &serde_json::to_vec(&json!({"success":true,"result":result,"errors":[]})).unwrap(),
    )
    .unwrap();
    assert_eq!(serde_json::from_slice::<Value>(&bytes).unwrap(), result);
    assert!(answer(
        DecisionFamily::Cloudflare,
        "https://api.cloudflare.com/client/v4",
        br#"{"success":false,"errors":[{"message":"refused"}]}"#
    )
    .is_err());
    assert!(answer(
        DecisionFamily::Qwen,
        "https://maas.qianwenaiapi.com/compatible-mode/v1",
        br#"{"answers":{}}"#
    )
    .is_err());
    assert!(answer(
        DecisionFamily::Cloudflare,
        "https://api.cloudflare.com/client/v4",
        br#"{"success":true,"result":{"choices":[]}}"#
    )
    .is_err());
}

#[test]
fn saved_vercel_evaluation_endpoints_map_noul_and_model_headers() {
    let original = question("typesafe-ai/jev");
    let (target, bytes, headers) = ask(
        DecisionFamily::Vercel,
        "https://ai-gateway.vercel.sh/v4/ai",
        serde_json::to_vec(&original).unwrap(),
    )
    .unwrap();
    let value: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(
        target,
        "https://ai-gateway.vercel.sh/v4/ai/evaluation-model"
    );
    assert_eq!(headers["ai-model-id"], "typesafe-ai/jev");
    assert_eq!(headers["ai-evaluation-model-specification-version"], "4");
    assert!(value.get("model").is_none());
    assert_eq!(value["questions"]["urgent"]["type"], "boolean");
    assert_eq!(value["questions"]["intent"]["type"], "choice");
}

#[test]
fn saved_vercel_evaluation_answers_restore_native_confidence_and_usage_names() {
    let bytes=answer(DecisionFamily::Vercel,"https://ai-gateway.vercel.sh/v4/ai",&serde_json::to_vec(&json!({"model":"typesafe-ai/jev","answers":{"urgent":{"type":"boolean","probability":0.9},"intent":{"type":"choice","choice":"a","probabilities":{"a":0.87,"b":0.1,"c":0.03}},"preserved":{"type":"choice","confidence":0.2,"probabilities":{"a":0.9,"b":0.1}}},"usage":{"inputTokens":19,"outputTokens":2},"provider_metadata":{"gateway":{"cost":"0.001"}}})).unwrap()).unwrap();
    let value: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(value["answers"]["urgent"]["noul"], 0.9);
    assert_eq!(value["answers"]["urgent"]["type"], "noul");
    assert!((value["answers"]["intent"]["confidence"].as_f64().unwrap() - 0.805).abs() < 0.000001);
    assert_eq!(value["answers"]["preserved"]["confidence"], 0.2);
    assert_eq!(value["usage"]["input_tokens"], 19);
    assert_eq!(value["usage"]["output_tokens"], 2);
    assert_eq!(value["provider_metadata"]["gateway"]["cost"], "0.001");
    assert_eq!(value["usage"]["cost"]["total"], 0.001);
}

#[test]
fn vercel_reports_only_actual_gateway_cost_and_keeps_existing_usage_cost() {
    let base = "https://ai-gateway.vercel.sh/typesafe/v1";
    for metadata in [Value::Null, json!({"cost":"NaN"}), json!({"cost":"-1"})] {
        let bytes=answer(DecisionFamily::Vercel,base,&serde_json::to_vec(&json!({"answers":{"q":{"noul":0.9}},"usage":{"input_tokens":12},"provider_metadata":{"gateway":metadata}})).unwrap()).unwrap();
        let value: Value = serde_json::from_slice(&bytes).unwrap();
        assert!(value["usage"].get("cost").is_none());
    }
    let bytes=answer(DecisionFamily::Vercel,base,&serde_json::to_vec(&json!({"answers":{"q":{"noul":0.9}},"usage":{"input_tokens":12,"cost":{"total":0.02}},"provider_metadata":{"gateway":{"cost":"0.04"}}})).unwrap()).unwrap();
    let value: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(value["usage"]["cost"]["total"], 0.02);
}
