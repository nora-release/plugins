use super::*;
use base64::{engine::general_purpose::STANDARD, Engine};
use serde_json::json;

struct Directory(std::path::PathBuf);

impl Drop for Directory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn final_codex_requests_match_the_reference_with_isolated_sign_in_paths() {
    let directory = Directory(std::env::temp_dir().join(format!(
        "nora-codex-request-parity-{}",
        uuid::Uuid::new_v4()
    )));
    std::fs::create_dir_all(&directory.0).unwrap();
    let module = module_path!().split_once("::").unwrap().1;
    let worker = format!("{module}::final_codex_request_worker");
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", &worker, "--ignored", "--nocapture"])
        .env_clear()
        .env("HOME", &directory.0)
        .env("CODEX_HOME", &directory.0)
        .env("PATH", "")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("running 1 test"));
}

async fn build(
    definition: &Definition,
    body: Value,
    headers: BTreeMap<String, String>,
    credentials: Value,
    url: &str,
) -> nora_ai::HttpRequest {
    let request = serde_json::from_value(json!({
        "account":"parity-fixture",
        "credentials":credentials,
        "subscriptionAccountsEnabled":true,
        "url":url,
        "method":"POST",
        "headers":headers,
        "body":STANDARD.encode(serde_json::to_vec(&body).unwrap()),
    }))
    .unwrap();
    nora_provider::transport::build_request(definition, request)
        .await
        .unwrap()
}

fn decoded(request: &nora_ai::HttpRequest) -> Value {
    let body = request.body.as_deref().unwrap();
    let plain = zstd::stream::decode_all(body).unwrap();
    serde_json::from_slice(&plain).unwrap()
}

fn header<'a>(request: &'a nora_ai::HttpRequest, name: &str) -> &'a str {
    request
        .headers
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case(name))
        .map(|(_, value)| value.as_str())
        .unwrap_or_else(|| panic!("Missing request header: {name}"))
}

#[tokio::test]
#[ignore = "isolated by final_codex_requests_match_the_reference_with_isolated_sign_in_paths"]
async fn final_codex_request_worker() {
    let mut definition =
        Definition::parse(serde_json::from_str(include_str!("../nora-plugin.json")).unwrap())
            .unwrap();
    definition.adapter = std::sync::Arc::new(OpenAI);
    OpenAI
        .initialize_data_directory(std::path::Path::new(&std::env::var("HOME").unwrap()))
        .unwrap();
    let credentials = json!({
        "type":"oauth", "credentialProvider":"openAICodex",
        "access":"synthetic-access", "expires":4070908800000_i64,
        "extras":{"accountId":"fixture-workspace"}
    });
    let prompt = "You are Codex, a coding agent.\nInstructions from the model list.";
    let entry = json!({"slug":"parity-model", "base_instructions":prompt});
    let mut model = Model::new("parity-model", "chatgpt-codex", "openai");
    OpenAI.enrich_model(&entry, &mut model, &credentials);
    let first = json!({
        "model":"parity-model", "instructions":"Nora task policy",
        "input":[{"type":"message", "role":"user", "content":[{"type":"input_text", "text":"hello"}]}],
        "temperature":0.5, "top_p":0.9, "max_output_tokens":100,
        "store":true, "stream":false,
        "reasoning":{"effort":"high"}
    });
    let target = "https://chatgpt.com/backend-api/codex/responses";
    let built = build(
        &definition,
        first.clone(),
        BTreeMap::from([
            ("authorization".into(), "Bearer caller-key".into()),
            ("originator".into(), "nora".into()),
            ("user-agent".into(), "Nora/old".into()),
            ("session_id".into(), "old-session".into()),
            ("Conversation_Id".into(), "old-conversation".into()),
        ]),
        credentials.clone(),
        target,
    )
    .await;
    assert_eq!(header(&built, "authorization"), "Bearer synthetic-access");
    assert_eq!(built.headers["chatgpt-account-id"], "fixture-workspace");
    assert_eq!(built.headers["originator"], "codex_cli_rs");
    let version = &built.headers["version"];
    assert!(built.headers["user-agent"].starts_with(&format!("codex_cli_rs/{version} (")));
    assert!(!built.headers["user-agent"].ends_with(" Nora"));
    assert_eq!(built.headers["openai-beta"], "responses=experimental");
    assert_eq!(built.headers["content-encoding"], "zstd");
    assert!(!built.headers.keys().any(|name| {
        ["session_id", "conversation_id"].contains(&name.to_ascii_lowercase().as_str())
    }));
    let body = decoded(&built);
    assert_eq!(body["instructions"], prompt);
    assert_eq!(body["input"][0]["role"], "developer");
    assert_eq!(body["input"][0]["content"][0]["text"], "Nora task policy");
    assert_eq!(body["input"][1], first["input"][0]);
    assert_eq!(body["stream"], true);
    assert_eq!(body["store"], false);
    assert_eq!(body["include"], json!(["reasoning.encrypted_content"]));
    for key in ["temperature", "top_p", "max_output_tokens"] {
        assert!(body.get(key).is_none(), "{key}");
    }
    let key = body["prompt_cache_key"].as_str().unwrap();
    assert_eq!(built.headers["session-id"], key);
    assert_eq!(built.headers["thread-id"], key);

    let mut second = first.clone();
    second["input"].as_array_mut().unwrap().extend([
        json!({"type":"function_call", "call_id":"call-tool-1", "name":"read_file", "arguments":"{}"}),
        json!({"type":"function_call_output", "call_id":"call-tool-1", "output":"file text"}),
        json!({"type":"message", "role":"user", "content":[{"type":"input_text", "text":"continue"}]}),
    ]);
    let continued = build(
        &definition,
        second,
        BTreeMap::new(),
        credentials.clone(),
        target,
    )
    .await;
    let continued_body = decoded(&continued);
    assert_eq!(continued_body["prompt_cache_key"], key);
    assert_eq!(continued.headers["session-id"], key);
    assert_eq!(continued_body["input"][2]["call_id"], "call-tool-1");
    assert_eq!(continued_body["input"][3]["call_id"], "call-tool-1");

    let native = json!({"model":"parity-model", "instructions":format!("{}\nNative Codex settings.", prompt.lines().next().unwrap()), "prompt_cache_key":"native-thread", "input":first["input"]});
    let native_request = build(
        &definition,
        native.clone(),
        BTreeMap::new(),
        credentials,
        target,
    )
    .await;
    let native_body = decoded(&native_request);
    assert_eq!(native_body["instructions"], native["instructions"]);
    assert_eq!(native_body["input"], native["input"]);
    assert_eq!(native_request.headers["session-id"], "native-thread");

    let api_body =
        json!({"model":"api-model", "instructions":"API policy", "input":"hello", "store":true});
    let api_request = build(
        &definition,
        api_body.clone(),
        BTreeMap::new(),
        json!({"type":"apiKey", "access":"synthetic-api-key"}),
        "https://api.openai.com/v1/responses",
    )
    .await;
    assert_eq!(
        serde_json::from_slice::<Value>(api_request.body.as_deref().unwrap()).unwrap(),
        api_body
    );
    assert!(!api_request.headers.contains_key("originator"));
    assert!(!api_request.headers.contains_key("content-encoding"));
}
