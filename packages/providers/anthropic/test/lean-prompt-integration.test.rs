use super::*;

async fn ask(
    host: &mut Host,
    credentials: &Value,
    session: &str,
    enabled: Option<bool>,
    system: Value,
    tools: Value,
    choice: Value,
    headers: Value,
    messages: Vec<Value>,
) -> Value {
    let mut metadata = json!({"sessionID":session});
    if let Some(enabled) = enabled {
        metadata["claudeLeanPrompt"] = json!(enabled.to_string());
    }
    let mut body = json!({"model":"claude-test","max_tokens":100,"system":system,"messages":messages,"tools":tools});
    if !choice.is_null() {
        body["tool_choice"] = choice;
    }
    let id=host.send("fetch",json!({"account":"work","credentials":credentials,"networkProxy":{"mode":"direct"},"url":"https://api.anthropic.com/v1/messages","method":"POST","headers":headers,"metadata":metadata,"body":STANDARD.encode(body.to_string())})).await;
    let (status, body) = host.response(id).await;
    assert_eq!(status, 200, "{body}");
    serde_json::from_str(&body).unwrap()
}
fn runs(account: &std::path::Path) -> Vec<Value> {
    logs(account)
        .into_iter()
        .filter(|log| log["args"][0] == "-p")
        .collect()
}
fn told(account: &std::path::Path, run: &Value) -> String {
    std::fs::read_to_string(account.join(format!("fixture-input-{}.json", run["pid"]))).unwrap()
}

#[tokio::test]
async fn lean_system_prompt_is_a_private_file_and_not_duplicated_in_the_user_turn() {
    let directory = fixture::Directory::new();
    let account = directory.0.join("claude-accounts/account-a");
    let credentials = fixture::profile(&account);
    let mut host = Host::new(&directory.0).await;
    for (n, system, tools, choice, expected) in [
        (
            "one",
            json!([{"type":"text","text":"x-anthropic-billing-header: cc_version=2.1.301.eca; cc_entrypoint=cli; cch=00000;"},{"type":"text","text":"You are Aria, a notes assistant."}]),
            json!([]),
            Value::Null,
            "You are Aria, a notes assistant.",
        ),
        (
            "two",
            json!("Rules."),
            tools(),
            json!({"type":"any"}),
            "Rules.\nYou must call at least one available tool before answering.",
        ),
        (
            "three",
            json!("Rules."),
            tools(),
            json!({"type":"tool","name":"lookup.tool"}),
            "Rules.\nYou must call the mcp__nora__lookup_tool_db2d36c00cfa tool.",
        ),
        ("empty", Value::Null, json!([]), Value::Null, ""),
    ] {
        let before = runs(&account);
        ask(
            &mut host,
            &credentials,
            n,
            Some(true),
            system,
            tools,
            choice,
            json!({}),
            vec![user(n)],
        )
        .await;
        let run = runs(&account)
            .into_iter()
            .find(|run| !before.iter().any(|old| old["pid"] == run["pid"]))
            .unwrap();
        assert_eq!(run["systemPrompt"], expected, "{run}");
        assert_eq!(run["systemMode"], 0o600);
        let words = told(&account, &run);
        for secret in [
            "external_system_instructions",
            "Aria",
            "Rules.",
            "must call",
            "x-anthropic-billing-header",
        ] {
            assert!(!words.contains(secret), "{words}");
        }
        assert!(words.contains(n));
    }
    drop(host);
}

#[tokio::test]
async fn default_and_disabled_settings_keep_the_normal_claude_prompt() {
    let directory = fixture::Directory::new();
    let account = directory.0.join("claude-accounts/account-a");
    let credentials = fixture::profile(&account);
    let mut host = Host::new(&directory.0).await;
    for (session, enabled) in [("default", None), ("off", Some(false))] {
        ask(
            &mut host,
            &credentials,
            session,
            enabled,
            json!("Rules."),
            json!([]),
            Value::Null,
            json!({}),
            vec![user(session)],
        )
        .await;
    }
    for run in runs(&account) {
        assert!(run["systemPrompt"].is_null());
        assert!(!run["args"]
            .as_array()
            .unwrap()
            .iter()
            .any(|arg| arg == "--system-prompt-file"));
        assert!(told(&account, &run).contains("external_system_instructions"));
    }
}

#[tokio::test]
async fn explicit_client_cache_marks_keep_the_normal_prompt_even_when_lean_is_enabled() {
    let directory = fixture::Directory::new();
    let account = directory.0.join("claude-accounts/account-a");
    let credentials = fixture::profile(&account);
    let mut host = Host::new(&directory.0).await;
    for (name, value) in [
        ("X-Magpie-Cache-Marks", "client"),
        ("x-magpie-cache-marks", "client"),
        ("X-Nora-Cache-Marks", "client"),
    ] {
        let mut headers = json!({});
        headers[name] = json!(value);
        ask(
            &mut host,
            &credentials,
            name,
            Some(true),
            json!([{"type":"text","text":"Rules.","cache_control":{"type":"ephemeral"}}]),
            json!([]),
            Value::Null,
            headers,
            vec![user("cache")],
        )
        .await;
    }
    for run in runs(&account) {
        assert!(run["systemPrompt"].is_null(), "{run}");
        assert!(told(&account, &run).contains("external_system_instructions"));
    }
}

#[tokio::test]
async fn changing_the_setting_restarts_the_session_but_an_unchanged_prompt_continues() {
    let directory = fixture::Directory::new();
    let account = directory.0.join("claude-accounts/account-a");
    let credentials = fixture::profile(&account);
    let mut host = Host::new(&directory.0).await;
    let mut messages = vec![user("one")];
    for (index, enabled) in [true, true, false, false, true].into_iter().enumerate() {
        let reply = ask(
            &mut host,
            &credentials,
            "same-session",
            Some(enabled),
            json!("Rules."),
            json!([]),
            Value::Null,
            json!({}),
            messages.clone(),
        )
        .await;
        messages.push(assistant(&reply));
        messages.push(user(&format!("next {index}")));
        assert_eq!(runs(&account).len(), [1, 1, 2, 2, 3][index]);
    }
    let runs = runs(&account);
    assert_eq!(
        runs.iter()
            .filter(|run| !run["systemPrompt"].is_null())
            .count(),
        2
    );
}
