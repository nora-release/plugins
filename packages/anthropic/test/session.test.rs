use super::*;
#[tokio::test]
async fn session_reuse_changes_when_proxy_or_prompt_ownership_changes() {
    let proxy = Proxy::new(None).await.unwrap();
    let request = json!({"model":"claude-test","system":"one","messages":[],"tools":[]});
    let mut changed = request.clone();
    changed["system"] = json!("two");
    assert_ne!(signature(&request, &proxy), signature(&changed, &proxy));
    let other = Proxy::new(Some(&nora_provider_plugin_sdk::NetworkProxy::Proxy {
        url: "http://127.0.0.1:8080".into(),
    }))
    .await
    .unwrap();
    assert_ne!(signature(&request, &proxy), signature(&request, &other));
}

#[tokio::test]
async fn cli_session_identity_tracks_search_capability_instead_of_wire_version_markers() {
    let proxy = Proxy::new(None).await.unwrap();
    let mut request = json!({"model":"claude-test","tools":[{"type":"web_search_20250305","name":"web_search"},{"name":"read","input_schema":{"type":"object"}}]});
    let first = signature(&request, &proxy);
    request["tools"][0] = json!({"type":"web_search_20260318","name":"web_search","max_uses":null});
    assert_eq!(first, signature(&request, &proxy));
    request["tools"][1]["input_schema"] = json!({"type":"object","required":["path"]});
    assert_ne!(first, signature(&request, &proxy));
    request["tools"] = json!([{"name":"read","input_schema":{"type":"object"}}]);
    assert_ne!(first, signature(&request, &proxy));
}

#[tokio::test]
async fn different_tool_choices_cannot_share_a_cli_run() {
    let proxy = Proxy::new(None).await.unwrap();
    let mut request = json!({"model":"claude-test","tools":[{"name":"read","input_schema":{"type":"object"}}],"tool_choice":{"type":"auto"}});
    let first = signature(&request, &proxy);
    request["tool_choice"] = json!({"type":"none"});
    assert_ne!(first, signature(&request, &proxy));
    request["tool_choice"] = json!({"type":"tool","name":"read"});
    assert_ne!(first, signature(&request, &proxy));
}

fn saved_fixture(root: &Path) -> (Saved, PathBuf, Vec<Value>) {
    let account = cli::Account {
        executable: root.join("claude"),
        home: cli::Home::Isolated(root.into()),
        identity: "account-a".into(),
        email: "person@example.test".into(),
        organization: "org-a".into(),
        plan: "max".into(),
        saved: json!({}),
        session_binding: "login-a".into(),
    };
    let work = Path::new("/private/tmp/claude-work-test");
    let project = root
        .canonicalize()
        .unwrap()
        .join("projects")
        .join(cli_sessions::project_name(work));
    cli::directory(&project).unwrap();
    let path = project.join("saved-a.jsonl");
    cli::write(&path, b"conversation").unwrap();
    let told = vec![
        json!({"role":"user","content":"first"}),
        json!({"role":"assistant","content":[{"type":"text","text":"answer"}]}),
    ];
    let mut incoming = told.clone();
    incoming.push(json!({"role":"user","content":"second"}));
    let saved = Saved {
        owner: cli_sessions::owner(&account),
        login: "login-a".into(),
        signature: "settings-a".into(),
        told,
        history: super::super::cli_messages::History::default(),
        idle: std::time::Instant::now(),
        session: cli_sessions::Session::capture(&account, work, "saved-a").unwrap(),
    };
    (saved, path, incoming)
}

#[test]
fn disk_resume_requires_the_same_login_settings_and_unmodified_history_prefix() {
    let directory = crate::fixture::Directory::new();
    let (saved, _, incoming) = saved_fixture(&directory.0);
    assert!(saved.matches(&saved.owner, "login-a", "settings-a", &incoming));
    assert!(!saved.matches("another-account", "login-a", "settings-a", &incoming));
    assert!(!saved.matches(&saved.owner, "another-login", "settings-a", &incoming));
    assert!(!saved.matches(&saved.owner, "login-a", "another-model-or-tools", &incoming));
    assert!(!saved.matches(&saved.owner, "login-a", "settings-a", &incoming[..2]));
    let mut modified = incoming;
    modified[0]["content"] = json!("rewritten");
    assert!(!saved.matches(&saved.owner, "login-a", "settings-a", &modified));
}

#[test]
fn shelf_expiry_deletes_the_owned_session_file() {
    let directory = crate::fixture::Directory::new();
    let (mut saved, path, _) = saved_fixture(&directory.0);
    saved.idle = std::time::Instant::now() - IDLE_LONGEST - std::time::Duration::from_secs(1);
    let mut pool = PoolState::default();
    pool.shelf.insert("conversation".into(), saved);
    pool.expire();
    assert!(pool.shelf.is_empty());
    assert!(!path.exists());
}

#[tokio::test]
async fn effort_changes_reuse_the_run_but_output_schema_changes_do_not() {
    let proxy = Proxy::new(None).await.unwrap();
    let mut request = json!({"model":"claude-test","tools":[]});
    let first = signature(&request, &proxy);
    request["output_config"] = json!({"effort":"low"});
    assert_eq!(first, signature(&request, &proxy));
    request["output_config"]["effort"] = json!("high");
    assert_eq!(first, signature(&request, &proxy));
    request["output_config"]["format"] = json!({"type":"json_schema","schema":{"type":"object"}});
    assert_ne!(first, signature(&request, &proxy));
}
