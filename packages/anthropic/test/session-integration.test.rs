use super::*;

fn runs(account: &std::path::Path) -> Vec<Value> {
    logs(account)
        .into_iter()
        .filter(|log| log["args"][0] == "-p")
        .collect()
}
fn fresh_run(account: &std::path::Path, old: &[Value]) -> Value {
    let fresh = runs(account)
        .into_iter()
        .filter(|run| !old.iter().any(|old| old["pid"] == run["pid"]))
        .collect::<Vec<_>>();
    assert_eq!(fresh.len(), 1, "{fresh:?}");
    fresh[0].clone()
}
fn input(account: &std::path::Path, run: &Value) -> String {
    std::fs::read_to_string(account.join(format!(
        "fixture-input-{}.json",
        run["pid"].as_u64().unwrap()
    )))
    .unwrap()
}
fn session_file(account: &std::path::Path, run: &Value) -> std::path::PathBuf {
    let project = run["cwd"]
        .as_str()
        .unwrap()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect::<String>();
    account.join("projects").join(project).join(format!(
        "fixture-session-{}.jsonl",
        run["pid"].as_u64().unwrap()
    ))
}
async fn answer(host: &mut Host, credentials: Value, session: &str, messages: Vec<Value>) -> Value {
    let id = host
        .fetch(credentials, session, "one", messages, json!([]), false)
        .await;
    let (status, body) = host.response(id).await;
    assert_eq!(status, 200, "{body}");
    serde_json::from_str(&body).unwrap()
}

#[tokio::test]
async fn idle_cli_sessions_resume_only_the_new_tail_and_fall_back_once_before_output() {
    let directory = fixture::Directory::new();
    let account = directory.0.join("claude-accounts/account-a");
    let credentials = fixture::profile(&account);
    let mut host = Host::new(&directory.0).await;
    let mut histories = Vec::new();
    let mut originals = Vec::new();
    for n in 0..20 {
        let old = runs(&account);
        let mut messages = vec![user(&format!("original question {n}"))];
        let response = answer(
            &mut host,
            credentials.clone(),
            &format!("session-{n}"),
            messages.clone(),
        )
        .await;
        messages.push(assistant(&response));
        messages.push(user(&format!("new question {n}")));
        histories.push(messages);
        originals.push(fresh_run(&account, &old));
    }
    assert!(originals.iter().all(|run| {
        !run["args"]
            .as_array()
            .unwrap()
            .iter()
            .any(|arg| arg == "--no-session-persistence")
            && run["cwd"] == originals[0]["cwd"]
    }));
    assert!(session_file(&account, &originals[0]).is_file());

    let old = runs(&account);
    answer(
        &mut host,
        credentials.clone(),
        "session-0",
        histories[0].clone(),
    )
    .await;
    let resumed = fresh_run(&account, &old);
    assert!(resumed["args"].as_array().unwrap().windows(2).any(|args| {
        args[0] == "--resume"
            && args[1] == format!("fixture-session-{}", originals[0]["pid"].as_u64().unwrap())
    }));
    let words = input(&account, &resumed);
    assert!(words.contains("new question 0"));
    assert!(!words.contains("original question") && !words.contains("Use the supplied tools only"));

    std::fs::write(account.join("fixture-fail-resume"), "").unwrap();
    let old = runs(&account);
    answer(
        &mut host,
        credentials.clone(),
        "session-1",
        histories[1].clone(),
    )
    .await;
    let fresh = runs(&account)
        .into_iter()
        .filter(|run| !old.iter().any(|old| old["pid"] == run["pid"]))
        .collect::<Vec<_>>();
    assert_eq!(fresh.len(), 2, "one failed restore, one cold startup");
    let cold = fresh
        .iter()
        .find(|run| {
            !run["args"]
                .as_array()
                .unwrap()
                .iter()
                .any(|arg| arg == "--resume")
        })
        .unwrap();
    let words = input(&account, cold);
    assert!(words.contains("original question 1") && words.contains("new question 1"));
    assert!(!session_file(&account, &originals[1]).exists());
    std::fs::remove_file(account.join("fixture-fail-resume")).unwrap();

    std::fs::remove_file(session_file(&account, &originals[2])).unwrap();
    let old = runs(&account);
    answer(
        &mut host,
        credentials.clone(),
        "session-2",
        histories[2].clone(),
    )
    .await;
    let cold = fresh_run(&account, &old);
    assert!(!cold["args"]
        .as_array()
        .unwrap()
        .iter()
        .any(|arg| arg == "--resume"));
    assert!(input(&account, &cold).contains("original question 2"));

    // A new login for the same account cannot inherit a previous login's shelf.
    let path = account.join(".credentials.json");
    let mut oauth: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    oauth["claudeAiOauth"]["accessToken"] = json!("new-login-access");
    oauth["claudeAiOauth"]["refreshToken"] = json!("new-login-refresh");
    std::fs::write(&path, oauth.to_string()).unwrap();
    let old = runs(&account);
    answer(
        &mut host,
        credentials.clone(),
        "session-3",
        histories[3].clone(),
    )
    .await;
    let cold = fresh_run(&account, &old);
    assert!(!cold["args"]
        .as_array()
        .unwrap()
        .iter()
        .any(|arg| arg == "--resume"));
    assert!(!session_file(&account, &originals[3]).exists());
    assert!(input(&account, &cold).contains("original question 3"));

    let other = directory.0.join("claude-accounts/account-b");
    let other_credentials = fixture::profile_with_identity(&other, "account-b");
    answer(
        &mut host,
        other_credentials,
        "session-0",
        histories[0].clone(),
    )
    .await;
    let other_runs = runs(&other);
    assert_eq!(other_runs.len(), 1);
    assert_ne!(other_runs[0]["cwd"], originals[0]["cwd"]);
    assert!(!other_runs[0]["args"]
        .as_array()
        .unwrap()
        .iter()
        .any(|arg| arg == "--resume"));
}

fn controls(account: &std::path::Path, run: &Value) -> Vec<Value> {
    std::fs::read_to_string(account.join(format!(
        "fixture-controls-{}.jsonl",
        run["pid"].as_u64().unwrap()
    )))
    .unwrap_or_default()
    .lines()
    .map(|line| serde_json::from_str(line).unwrap())
    .collect()
}

async fn effort_answer(
    host: &mut Host,
    credentials: Value,
    session: &str,
    messages: Vec<Value>,
    effort: &str,
) -> Value {
    let body = json!({"model":"claude-test","max_tokens":1024,"stream":false,"system":"Use the supplied tools only.","messages":messages,"tools":[],"output_config":{"effort":effort}});
    let id = host.send("fetch", json!({"account":"work","credentials":credentials,"networkProxy":{"mode":"direct"},"url":"https://api.anthropic.com/v1/messages","method":"POST","headers":{},"metadata":{"sessionID":session,"turnID":effort},"body":STANDARD.encode(body.to_string())})).await;
    let (status, body) = host.response(id).await;
    assert_eq!(status, 200, "{body}");
    serde_json::from_str(&body).unwrap()
}

#[tokio::test]
async fn effort_changes_apply_native_flags_in_the_same_cli_and_only_send_new_user_words() {
    let directory = fixture::Directory::new();
    let account = directory.0.join("claude-accounts/account-a");
    let credentials = fixture::profile(&account);
    let mut host = Host::new(&directory.0).await;
    let mut history = vec![user("original cached question")];
    let first = effort_answer(
        &mut host,
        credentials.clone(),
        "effort-session",
        history.clone(),
        "low",
    )
    .await;
    history.push(assistant(&first));
    history.push(user("new high effort question"));
    let second = effort_answer(
        &mut host,
        credentials.clone(),
        "effort-session",
        history.clone(),
        "high",
    )
    .await;
    assert_eq!(runs(&account).len(), 1);
    let run = &runs(&account)[0];
    assert_eq!(
        controls(&account, run)[0]["request"],
        json!({"subtype":"apply_flag_settings","settings":{"effortLevel":"high"}})
    );
    assert!(input(&account, run).contains("new high effort question"));
    assert!(!input(&account, run).contains("original cached question"));

    std::fs::write(account.join("fixture-refuse-effort"), "").unwrap();
    history.push(assistant(&second));
    history.push(user("new default effort question"));
    answer(&mut host, credentials, "effort-session", history).await;
    assert_eq!(
        runs(&account).len(),
        2,
        "a CLI refusing the control safely cold-starts once"
    );
    let cold = runs(&account)
        .into_iter()
        .find(|other| other["pid"] != run["pid"])
        .unwrap();
    assert!(input(&account, &cold).contains("original cached question"));
}

#[tokio::test]
async fn cancelling_a_resumed_turn_rewinds_native_uuid_and_retry_keeps_only_the_cached_tail() {
    for (refused, conversation) in [
        (false, "rewind-session"),
        (true, "rewind-session"),
        (false, ""),
    ] {
        let directory = fixture::Directory::new();
        let account = directory.0.join("claude-accounts/account-a");
        let credentials = fixture::profile(&account);
        let mut host = Host::new(&directory.0).await;
        let mut history = vec![user("original cached question")];
        let first = answer(
            &mut host,
            credentials.clone(),
            conversation,
            history.clone(),
        )
        .await;
        history.push(assistant(&first));
        history.push(user("WAIT_REWIND new question"));
        if refused {
            std::fs::write(account.join("fixture-refuse-rewind"), "").unwrap();
        }
        let waiting = host
            .fetch(
                credentials.clone(),
                conversation,
                "next-turn",
                history.clone(),
                json!([]),
                true,
            )
            .await;
        loop {
            let frame = host.frame(waiting).await;
            if frame["event"] == "chunk"
                && String::from_utf8(STANDARD.decode(frame["data"].as_str().unwrap()).unwrap())
                    .unwrap()
                    .contains("partial cancelled answer")
            {
                break;
            }
            assert!(frame.get("error").is_none(), "{frame}");
        }
        let abort = host.send("abort", json!({"id":waiting})).await;
        loop {
            let frame = host.frame(waiting).await;
            if frame.get("error").is_some() {
                assert_eq!(frame["error"]["code"], "cancelled");
                break;
            }
        }
        assert_eq!(host.frame(abort).await["result"]["aborted"], true);
        let response = answer(&mut host, credentials, conversation, history).await;
        assert!(!response.to_string().contains("partial cancelled answer"));
        let processes = runs(&account);
        assert_eq!(processes.len(), if refused { 2 } else { 1 });
        let rewind = processes
            .iter()
            .flat_map(|run| controls(&account, run))
            .find(|control| control["request"]["subtype"] == "rewind_conversation")
            .expect("real native rewind request");
        let uuid = std::fs::read_to_string(account.join("fixture-rewind-waiting")).unwrap();
        assert_eq!(rewind["request"]["target_message_uuid"], uuid);
        assert_eq!(rewind["request"]["interrupt_if_running"], true);
        if !refused {
            let words = input(&account, &processes[0]);
            assert!(words.contains("WAIT_REWIND new question"));
            assert!(!words.contains("original cached question"));
        }
    }
}
