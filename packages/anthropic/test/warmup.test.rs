use super::*;
#[tokio::test]
async fn warmup_requires_explicit_confirmation_before_any_cli_access() {
    let error = Claude::default()
        .call("warmup", json!({}))
        .await
        .unwrap_err();
    assert_eq!(error.code, "invalid_request");
    assert!(error.message.contains("Confirm"), "{error:?}");
}
#[test]
fn warmup_declares_omitted_idle_windows() {
    let manifest: Value = serde_json::from_str(include_str!("../nora-plugin.json")).unwrap();
    assert_eq!(
        manifest["contributes"]["provider"]["config"]["accountAutomation"]["warmup"]["method"],
        "warmup"
    );
    assert_eq!(
        manifest["contributes"]["provider"]["config"]["accountAutomation"]["warmup"]
            ["expectedWindows"][0]["id"],
        "five_hour"
    );
}
async fn fixture() -> (crate::fixture::Directory, Claude, Value, std::path::PathBuf) {
    let directory = crate::fixture::Directory::new();
    let plugin = Claude::default();
    plugin
        .initialize(InitializeParams {
            api_version: 2,
            plugin_id: "anthropic".into(),
            plugin_directory: directory.0.clone(),
            data_directory: directory.0.join("data"),
            manifest: serde_json::from_str(include_str!("../nora-plugin.json")).unwrap(),
            installed_manifest: None,
            runtime_config: Value::Null,
            subscription_accounts_enabled: true,
        })
        .await
        .unwrap();
    let profile = directory.0.join("data/claude-accounts/a");
    let credentials = crate::fixture::profile(&profile);
    (directory, plugin, credentials, profile)
}
#[tokio::test]
async fn warmup_uses_haiku_without_tools_sessions_or_user_working_directory() {
    let (directory, plugin, credentials, profile) = fixture().await;
    let result=plugin.call("warmup",json!({"account":"a","credentials":credentials,"confirmed":true,"networkProxy":{"mode":"direct"}})).await.unwrap();
    assert_eq!(result["outcome"], "warmed");
    let record: Value =
        serde_json::from_slice(&std::fs::read(profile.join("fixture-warmup.json")).unwrap())
            .unwrap();
    assert_eq!(record["input"], "hi");
    assert_eq!(
        record["args"],
        json!([
            "-p",
            "--output-format",
            "json",
            "--model",
            "haiku",
            "--tools",
            "",
            "--strict-mcp-config",
            "--setting-sources",
            "",
            "--no-session-persistence"
        ])
    );
    let work = std::path::PathBuf::from(record["cwd"].as_str().unwrap());
    assert!(work.starts_with(
        directory
            .0
            .canonicalize()
            .unwrap()
            .join("data/claude-accounts")
    ));
    assert_ne!(work, profile);
    assert!(!work.exists());
    assert!(!profile.join("projects").exists());
    assert!(profile.join(".credentials.json").exists());
    assert_eq!(plugin.pool.parked(&credentials).await, 0);
}
#[tokio::test]
async fn warmup_handles_verbose_json_and_cli_failures_without_losing_sign_in() {
    for (stdout, exit, ok) in [
        (
            json!([{"type":"assistant"},{"type":"result","is_error":false,"result":"hi"}])
                .to_string(),
            0,
            true,
        ),
        (
            json!({"type":"result","is_error":true,"result":"busy"}).to_string(),
            0,
            false,
        ),
        ("not json".into(), 0, false),
        (
            json!({"type":"result","is_error":false}).to_string(),
            1,
            false,
        ),
    ] {
        let (_directory, plugin, credentials, profile) = fixture().await;
        let before = std::fs::read(profile.join(".credentials.json")).unwrap();
        std::fs::write(
            profile.join("fixture-warmup-reply.json"),
            json!({"stdout":stdout,"exit":exit}).to_string(),
        )
        .unwrap();
        assert_eq!(
            plugin
                .call(
                    "warmup",
                    json!({"account":"a","credentials":credentials,"confirmed":true})
                )
                .await
                .is_ok(),
            ok
        );
        assert_eq!(
            std::fs::read(profile.join(".credentials.json")).unwrap(),
            before
        );
    }
}
#[tokio::test]
async fn warmup_does_not_dispatch_api_credentials_or_disabled_subscriptions() {
    let (_directory, plugin, credentials, profile) = fixture().await;
    for params in [
        json!({"account":"a","credentials":credentials,"confirmed":true,"subscriptionAccountsEnabled":false}),
        json!({"account":"a","credentials":{"type":"apiKey","key":"fake"},"confirmed":true}),
    ] {
        assert!(plugin.call("warmup", params).await.is_err());
    }
    assert!(!profile.join("fixture-warmup.json").exists());
}
#[tokio::test]
async fn cancelling_warmup_kills_cli_and_removes_only_temporary_work() {
    let (_directory, plugin, credentials, profile) = fixture().await;
    std::fs::write(profile.join("fixture-warmup-pause"), "").unwrap();
    let task = tokio::spawn(async move {
        plugin
            .call(
                "warmup",
                json!({"account":"a","credentials":credentials,"confirmed":true}),
            )
            .await
    });
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while !profile.join("fixture-warmup.json").exists() {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let record: Value =
        serde_json::from_slice(&std::fs::read(profile.join("fixture-warmup.json")).unwrap())
            .unwrap();
    task.abort();
    let _ = task.await;
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        while unsafe { libc::kill(record["pid"].as_i64().unwrap() as i32, 0) } == 0 {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert!(!std::path::Path::new(record["cwd"].as_str().unwrap()).exists());
    assert!(profile.join(".credentials.json").exists());
}
