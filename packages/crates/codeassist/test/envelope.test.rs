use super::*;
#[test]
fn claude_tools_have_paired_unique_results_safe_names_and_a_supported_schema() {
    let body = json!({"contents":[{"role":"model","parts":[{"thought":true,"text":"foreign thinking","thoughtSignature":"foreign"},{"functionCall":{"id":"a.1","name":"mcp/tool","args":{}}},{"functionCall":{"id":"b:2","name":"second","args":{}}}]},{"role":"function","parts":[{"functionResponse":{"id":"a.1","name":"mcp/tool","response":{"output":"one"}}},{"functionResponse":{"id":"a.1","name":"mcp/tool","response":{"output":"duplicate"}}},{"inlineData":{"mimeType":"image/png","data":"aW1hZ2U="}}]}],"tools":[{"functionDeclarations":[{"name":"mcp/tool","parameters":{"$defs":{"query":{"type":"string","format":"uri"}},"type":"object","properties":{"query":{"$ref":"#/$defs/query"}},"required":["query","missing"],"additionalProperties":false}}]}],"toolConfig":{"functionCallingConfig":{"mode":"AUTO"}}});
    let (env, names) = build(
        App::Antigravity,
        "claude-opus-4-6-thinking",
        &body,
        "project-a",
    )
    .unwrap();
    let rows = env["request"]["contents"].as_array().unwrap();
    assert_eq!(rows[0]["role"], "user");
    assert!(!env.to_string().contains("foreign thinking"));
    let calls = rows[1]["parts"].as_array().unwrap();
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0]["thoughtSignature"], SKIP);
    let wire = calls[0]["functionCall"]["name"].as_str().unwrap();
    assert_eq!(names.restore(wire), "mcp/tool");
    let answers = rows[2]["parts"].as_array().unwrap();
    assert_eq!(
        answers
            .iter()
            .filter(|part| part.get("functionResponse").is_some())
            .count(),
        2
    );
    assert_eq!(answers[0]["functionResponse"]["response"]["result"], "one");
    assert_eq!(
        answers[1]["functionResponse"]["response"]["result"],
        INTERRUPTED
    );
    assert!(answers[2].get("inlineData").is_some());
    assert_eq!(
        env["request"]["toolConfig"]["functionCallingConfig"]["mode"],
        "VALIDATED"
    );
    let schema = &env["request"]["tools"][0]["functionDeclarations"][0]["parameters"];
    assert_eq!(schema["properties"]["query"]["type"], "string");
    assert!(schema["properties"]["query"].get("format").is_none());
    assert_eq!(schema["required"], json!(["query"]));
    assert_eq!(body["contents"][0]["parts"][0]["text"], "foreign thinking");
}
#[test]
fn gemini_preserves_signatures_and_splits_tool_answers_from_the_next_user_prompt() {
    let body = json!({"contents":[{"role":"user","parts":[{"text":"start"}]},{"role":"model","parts":[{"text":"","thoughtSignature":"text-sig"},{"thought":true,"text":"thinking","thoughtSignature":"thought-sig"},{"functionCall":{"id":"a","name":"tool","args":{}},"thoughtSignature":"call-sig"}]},{"role":"user","parts":[{"functionResponse":{"id":"a","name":"tool","response":{"output":"done"}}},{"text":"continue"}]}]});
    let (env, _) = build(App::GeminiCLI, "gemini-3.8-flash", &body, "project-a").unwrap();
    let rows = env["request"]["contents"].as_array().unwrap();
    assert_eq!(rows.len(), 4);
    assert_eq!(rows[1]["parts"][0]["thoughtSignature"], "text-sig");
    assert_eq!(rows[1]["parts"][1]["thoughtSignature"], "thought-sig");
    assert_eq!(rows[1]["parts"][2]["thoughtSignature"], "call-sig");
    assert!(rows[2]["parts"][0].get("functionResponse").is_some());
    assert_eq!(rows[3]["parts"][0]["text"], "continue");
    assert!(env.get("user_prompt_id").is_some());
    assert!(env.get("requestType").is_none());
}
#[test]
fn schemas_resolve_definitions_flatten_combinations_and_bound_cycles() {
    let schema = json!({"$defs":{"cycle":{"$ref":"#/$defs/cycle"}},"allOf":[{"type":"object","properties":{"a":{"type":["string","null"],"const":"x"}},"required":["a"]},{"properties":{"b":{"anyOf":[{"type":"null"},{"type":"number","minimum":1}]}},"required":["b"]}],"title":"discard"});
    let plain = plain_schema(&schema);
    assert_eq!(plain["required"], json!(["a", "b"]));
    assert_eq!(plain["properties"]["a"]["nullable"], true);
    assert_eq!(plain["properties"]["a"]["enum"], json!(["x"]));
    assert_eq!(plain["properties"]["b"]["nullable"], true);
    assert!(plain.get("title").is_none());
    assert_eq!(
        plain_schema(&json!({"$defs":{"x":{"$ref":"#/$defs/x"}},"$ref":"#/$defs/x"}))["type"],
        "object"
    );
}
#[test]
fn session_ids_are_stable_and_claude_thinking_fits_its_answer_limit() {
    let body = json!({"contents":[{"role":"user","parts":[{"text":"same question"}]}],"generationConfig":{"thinkingConfig":{"thinkingBudget":20000},"maxOutputTokens":10000}});
    let (a, _) = build(App::Antigravity, "claude-x-thinking", &body, "p").unwrap();
    let (b, _) = build(App::Antigravity, "claude-x-thinking", &body, "p").unwrap();
    assert_eq!(a["request"]["sessionId"], b["request"]["sessionId"]);
    assert_ne!(a["requestId"], b["requestId"]);
    assert_eq!(
        a["request"]["generationConfig"]["thinkingConfig"]["thinkingBudget"],
        9999
    );
    let (c, _) = build(App::Antigravity, "gemini-3.8-flash-high", &body, "p").unwrap();
    assert_eq!(
        c["request"]["generationConfig"]["thinkingConfig"]["thinkingLevel"],
        "high"
    );
    assert!(c["request"]["generationConfig"]
        .get("maxOutputTokens")
        .is_none());
}

#[test]
fn private_effort_metadata_is_converted_only_to_the_selected_models_supported_format() {
    let body = json!({"contents":[{"role":"user","parts":[{"text":"hello"}]}]});
    let (gemini, _) = build_with_effort(
        App::Antigravity,
        "gemini-3.8-flash-high",
        &body,
        "p",
        Some("low"),
    )
    .unwrap();
    assert_eq!(
        gemini["request"]["generationConfig"]["thinkingConfig"]["thinkingLevel"],
        "high"
    );
    let (claude, _) = build_with_effort(
        App::Antigravity,
        "claude-opus-4-6-thinking",
        &body,
        "p",
        Some("high"),
    )
    .unwrap();
    assert!(claude["request"]["generationConfig"]["thinkingConfig"]
        .get("thinkingLevel")
        .is_none());
    assert_eq!(
        claude["request"]["generationConfig"]["thinkingConfig"]["thinkingBudget"],
        24000
    );
}

#[test]
fn file_parts_remain_exact_through_google_and_claude_tool_pairing() {
    let pdf = json!({"inlineData":{"mimeType":"application/pdf","data":"UERG","displayName":"report.pdf"}});
    let audio = json!({"fileData":{"mimeType":"audio/wav","fileUri":"gs://bucket/recording.wav","displayName":"recording.wav"}});
    let video = json!({"fileData":{"mimeType":"video/mp4","fileUri":"https://files.example.test/movie.mp4","displayName":"movie.mp4"}});
    let body = json!({"contents":[{"role":"model","parts":[{"functionCall":{"id":"read-1","name":"read","args":{}}}]},{"role":"user","parts":[{"functionResponse":{"id":"read-1","name":"read","response":{"output":"files"}}},pdf,audio,video]}]});
    for (app, model) in [
        (App::GeminiCLI, "gemini-2.5-pro"),
        (App::Antigravity, "gemini-2.5-pro"),
        (App::Antigravity, "claude-sonnet-4-6"),
    ] {
        let (envelope, _) = build(app, model, &body, "project").unwrap();
        let parts = envelope["request"]["contents"]
            .as_array()
            .unwrap()
            .iter()
            .flat_map(|row| row["parts"].as_array().unwrap())
            .collect::<Vec<_>>();
        assert!(parts.contains(&&pdf));
        assert!(parts.contains(&&audio));
        assert!(parts.contains(&&video));
    }
}
