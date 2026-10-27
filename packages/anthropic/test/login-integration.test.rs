use super::*;
use std::{path::Path, time::Duration};

fn events(root: &Path) -> Vec<Value> {
    std::fs::read_to_string(root.join("login-events.jsonl"))
        .unwrap_or_default()
        .lines()
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect()
}

async fn start(root: &Path, mode: &str, hold: bool) -> (Host, Value) {
    std::fs::write(
        root.join("login-fixture.json"),
        json!({"mode":mode,"hold":hold}).to_string(),
    )
    .unwrap();
    let bin = root.join("test-bin");
    std::fs::create_dir_all(&bin).unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink(fixture::binary(), bin.join("security")).unwrap();
    let mut paths = vec![bin];
    paths.extend(std::env::split_paths(
        &std::env::var_os("PATH").unwrap_or_default(),
    ));
    let mut host = Host::with_environment(
        root,
        &[
            ("PATH", std::env::join_paths(paths).unwrap()),
            ("NORA_FIXTURE_KEYCHAIN", root.as_os_str().to_owned()),
        ],
    )
    .await;
    let id = host.send("authorize", json!({"authMethodID":"oauth","inputs":{"claudePath":fixture::binary()},"networkProxy":{"mode":"direct"}})).await;
    let frame = host.frame(id).await;
    assert!(frame.get("error").is_none(), "{frame}");
    (host, frame["result"].clone())
}

async fn callback(host: &mut Host, session: &Value, input: Value) -> Value {
    let mut params = input.as_object().cloned().unwrap_or_default();
    params.insert("session".into(), session.clone());
    params.insert("poll".into(), json!(true));
    let id = host.send("callback", json!(params)).await;
    host.frame(id).await
}

async fn finished(host: &mut Host, session: &Value) -> Value {
    for _ in 0..300 {
        let frame = callback(host, session, json!({})).await;
        assert!(frame.get("error").is_none(), "{frame}");
        if frame["result"]["pending"] != true {
            return frame["result"].clone();
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("Claude did not finish its sign-in");
}

#[tokio::test]
async fn claude_manual_sign_in_advertises_code_input_while_polling_the_same_cli() {
    let directory = fixture::Directory::new();
    let (mut host, login) = start(&directory.0, "manual", false).await;
    assert_eq!(
        login["method"], "code",
        "the printed page needs a pasted code"
    );
    assert_eq!(login["callbackMode"], "poll");
    assert_eq!(login["pasteCode"], true);
    let pending = callback(&mut host, &login["session"], json!({})).await;
    assert_eq!(pending["result"]["session"], login["session"]);
    assert_eq!(pending["result"]["pending"], true);
    for code in [
        "not a code",
        "fixture-code#other-state",
        "fixture-code#fixture-state\nnext",
    ] {
        let reply = callback(&mut host, &login["session"], json!({"code":code})).await;
        assert_eq!(reply["result"]["pending"], true, "{reply}");
        assert!(reply["result"]["inputError"].is_string(), "{reply}");
        assert!(!events(&directory.0)
            .iter()
            .any(|event| event.get("code").is_some()));
    }
    let reply = callback(
        &mut host,
        &login["session"],
        json!({"code":"  fixture-code#fixture-state  "}),
    )
    .await;
    assert!(reply.get("error").is_none(), "{reply}");
    let account = if reply["result"]["pending"] == true {
        finished(&mut host, &login["session"]).await
    } else {
        reply["result"].clone()
    };
    assert_eq!(account["credentials"]["email"], "person@example.test");
    assert_eq!(
        events(&directory.0)
            .iter()
            .filter(|event| event.get("code").is_some())
            .count(),
        1
    );
    assert_eq!(
        events(&directory.0)
            .iter()
            .filter(|event| event["finished"] == true)
            .count(),
        1
    );
    assert!(callback(&mut host, &login["session"], json!({}))
        .await
        .get("error")
        .is_some());
}

#[tokio::test]
async fn claude_opened_sign_in_prefers_the_browser_callback_over_its_printed_code_page() {
    let directory = fixture::Directory::new();
    let (mut host, login) = start(&directory.0, "opened", false).await;
    assert_eq!(login["method"], "auto");
    assert_eq!(login["callbackMode"], "poll");
    assert_eq!(login["pasteCallback"], true);
    let page = url::Url::parse(login["url"].as_str().unwrap()).unwrap();
    let redirect = page
        .query_pairs()
        .find(|(key, _)| key == "redirect_uri")
        .unwrap()
        .1
        .into_owned();
    assert!(redirect.starts_with("http://localhost:"), "{login}");
    for address in [
        "http://localhost:1/callback?code=fixture-code&state=fixture-state".to_owned(),
        format!("{redirect}?code=fixture-code&state=other-state"),
        format!(
            "{}?code=fixture-code&state=fixture-state",
            redirect.replace("/callback", "/other")
        ),
        format!(
            "{}?code=fixture-code&state=fixture-state",
            redirect.replace("localhost", "outside.invalid")
        ),
    ] {
        let reply = callback(&mut host, &login["session"], json!({"callbackURL":address})).await;
        assert_eq!(reply["result"]["pending"], true, "{reply}");
        assert!(reply["result"]["inputError"].is_string(), "{reply}");
        assert!(!events(&directory.0)
            .iter()
            .any(|event| event.get("request").is_some()));
    }
    let reply = callback(
        &mut host,
        &login["session"],
        json!({"callbackURL":format!("{redirect}?code=fixture-code&state=fixture-state")}),
    )
    .await;
    assert!(reply.get("error").is_none(), "{reply}");
    let account = if reply["result"]["pending"] == true {
        finished(&mut host, &login["session"]).await
    } else {
        reply["result"].clone()
    };
    assert_eq!(account["credentials"]["email"], "person@example.test");
    assert_eq!(
        events(&directory.0)
            .iter()
            .filter(|event| event.get("request").is_some())
            .count(),
        1
    );
}

async fn wait_event(root: &Path, key: &str) -> Value {
    for _ in 0..300 {
        if let Some(event) = events(root)
            .into_iter()
            .find(|event| event.get(key).is_some())
        {
            return event;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("The CLI never reported {key}");
}

fn profile_directory(root: &Path) -> std::path::PathBuf {
    let event = events(root)
        .into_iter()
        .find(|event| event["ready"] == true)
        .unwrap();
    event["directory"].as_str().unwrap().into()
}

async fn removed(path: &Path) {
    for _ in 0..300 {
        if !path.exists() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!(
        "The canceled or failed sign-in kept its directory: {}",
        path.display()
    );
}

#[tokio::test]
async fn claude_manual_sign_in_automatic_success_keeps_the_account_without_input() {
    for mode in ["manual", "opened"] {
        let directory = fixture::Directory::new();
        let (mut host, login) = start(&directory.0, mode, false).await;
        let profile = profile_directory(&directory.0);
        std::fs::write(directory.0.join("finish-login"), b"done").unwrap();
        let result = finished(&mut host, &login["session"]).await;
        assert_eq!(result["credentials"]["email"], "person@example.test");
        assert!(profile.join(".credentials.json").is_file());
        assert!(directory.0.join("simulated-keychain").is_file());
        assert!(!events(&directory.0)
            .iter()
            .any(|event| event.get("code").is_some() || event.get("request").is_some()));
    }
}

#[tokio::test]
async fn claude_manual_sign_in_duplicate_input_and_racing_polls_finish_only_once() {
    let directory = fixture::Directory::new();
    let (mut host, login) = start(&directory.0, "manual", true).await;
    for _ in 0..2 {
        let reply = callback(
            &mut host,
            &login["session"],
            json!({"code":"fixture-code#fixture-state"}),
        )
        .await;
        assert_eq!(reply["result"]["pending"], true, "{reply}");
        assert!(reply["result"].get("inputError").is_none(), "{reply}");
    }
    wait_event(&directory.0, "accepted").await;
    assert_eq!(
        events(&directory.0)
            .iter()
            .filter(|event| event.get("code").is_some())
            .count(),
        1
    );
    std::fs::write(directory.0.join("release-login"), b"done").unwrap();
    wait_event(&directory.0, "finished").await;
    let first = host
        .send("callback", json!({"session":login["session"]}))
        .await;
    let second = host
        .send(
            "callback",
            json!({"session":login["session"],"code":"fixture-code#fixture-state"}),
        )
        .await;
    let replies = [host.frame(first).await, host.frame(second).await];
    assert!(profile_directory(&directory.0)
        .join(".credentials.json")
        .is_file());
    assert!(directory.0.join("simulated-keychain").exists());
    assert!(!directory.0.join("keychain-cleanup.json").exists());
    assert_eq!(
        replies
            .iter()
            .filter(|reply| reply["result"].get("credentials").is_some())
            .count(),
        1,
        "{replies:?}"
    );
    assert_eq!(
        replies
            .iter()
            .filter(|reply| reply.get("error").is_some())
            .count(),
        1,
        "{replies:?}"
    );
    assert_eq!(
        events(&directory.0)
            .iter()
            .filter(|event| event["finished"] == true)
            .count(),
        1
    );
}

#[tokio::test]
async fn claude_manual_sign_in_failure_cleans_the_isolated_profile() {
    let directory = fixture::Directory::new();
    let (mut host, login) = start(&directory.0, "manual", false).await;
    let profile = profile_directory(&directory.0);
    std::fs::write(directory.0.join("fail-login"), b"fail").unwrap();
    let id = host
        .send("callback", json!({"session":login["session"]}))
        .await;
    let reply = host.frame(id).await;
    assert!(reply.get("error").is_some(), "{reply}");
    removed(&profile).await;
    assert_private_keychain_removed(&directory.0, &profile);
    assert!(callback(&mut host, &login["session"], json!({}))
        .await
        .get("error")
        .is_some());
}

#[tokio::test]
async fn claude_manual_sign_in_cancel_cleans_waiting_and_accepted_code_without_completion() {
    for submitted in [false, true] {
        let directory = fixture::Directory::new();
        let (mut host, login) = start(&directory.0, "manual", true).await;
        let ready = wait_event(&directory.0, "ready").await;
        let profile = profile_directory(&directory.0);
        if submitted {
            let reply = callback(
                &mut host,
                &login["session"],
                json!({"code":"fixture-code#fixture-state"}),
            )
            .await;
            assert_eq!(reply["result"]["pending"], true);
            wait_event(&directory.0, "accepted").await;
        }
        for _ in 0..2 {
            let id = host
                .send("cancelAuthorization", json!({"session":login["session"]}))
                .await;
            assert_eq!(host.frame(id).await["result"]["ok"], true);
        }
        removed(&profile).await;
        assert_private_keychain_removed(&directory.0, &profile);
        std::fs::write(directory.0.join("release-login"), b"done").unwrap();
        assert!(callback(
            &mut host,
            &login["session"],
            json!({"code":"fixture-code#fixture-state"})
        )
        .await
        .get("error")
        .is_some());
        assert!(!events(&directory.0)
            .iter()
            .any(|event| event["finished"] == true));
        #[cfg(unix)]
        {
            let pid = ready["pid"].as_i64().unwrap() as i32;
            for _ in 0..300 {
                if unsafe { libc::kill(pid, 0) } == -1 {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            assert_eq!(unsafe { libc::kill(pid, 0) }, -1);
        }
    }
}

#[tokio::test]
async fn claude_manual_sign_in_cancellation_interrupts_an_in_flight_local_callback() {
    let directory = fixture::Directory::new();
    let (mut host, login) = start(&directory.0, "opened-stalled", false).await;
    let profile = profile_directory(&directory.0);
    let page = url::Url::parse(login["url"].as_str().unwrap()).unwrap();
    let redirect = page
        .query_pairs()
        .find(|(key, _)| key == "redirect_uri")
        .unwrap()
        .1
        .into_owned();
    let input = host.send("callback", json!({"session":login["session"],"poll":true,"callbackURL":format!("{redirect}?code=fixture-code&state=fixture-state")})).await;
    wait_event(&directory.0, "request").await;
    let cancel = host
        .send("cancelAuthorization", json!({"session":login["session"]}))
        .await;
    let reply = tokio::time::timeout(Duration::from_secs(3), host.frame(cancel))
        .await
        .unwrap();
    assert_eq!(reply["result"]["ok"], true, "{reply}");
    assert!(host.frame(input).await.get("error").is_some());
    removed(&profile).await;
    assert_private_keychain_removed(&directory.0, &profile);
    assert!(!events(&directory.0)
        .iter()
        .any(|event| event["finished"] == true));
}

#[tokio::test]
async fn claude_manual_sign_in_rejected_local_callback_preserves_the_session_for_retry() {
    let directory = fixture::Directory::new();
    let (mut host, login) = start(&directory.0, "opened", false).await;
    let page = url::Url::parse(login["url"].as_str().unwrap()).unwrap();
    let redirect = page
        .query_pairs()
        .find(|(key, _)| key == "redirect_uri")
        .unwrap()
        .1
        .into_owned();
    let reply = callback(
        &mut host,
        &login["session"],
        json!({"callbackURL":format!("{redirect}?code=wrong&state=fixture-state")}),
    )
    .await;
    assert_eq!(reply["result"]["pending"], true, "{reply}");
    assert!(reply["result"]["inputError"].is_string());
    assert!(profile_directory(&directory.0).exists());
    let reply = callback(
        &mut host,
        &login["session"],
        json!({"callbackURL":format!("{redirect}?code=fixture-code&state=fixture-state")}),
    )
    .await;
    assert!(reply.get("error").is_none());
    let result = if reply["result"]["pending"] == true {
        finished(&mut host, &login["session"]).await
    } else {
        reply["result"].clone()
    };
    assert_eq!(result["credentials"]["email"], "person@example.test");
    assert_eq!(
        events(&directory.0)
            .iter()
            .filter(|event| event.get("request").is_some())
            .count(),
        2
    );
}

#[cfg(target_os = "macos")]
fn assert_private_keychain_removed(root: &Path, profile: &Path) {
    use sha2::{Digest, Sha256};
    use unicode_normalization::UnicodeNormalization;
    let digest = format!(
        "{:x}",
        Sha256::digest(
            profile
                .to_string_lossy()
                .nfc()
                .collect::<String>()
                .as_bytes()
        )
    );
    let expected = format!("Claude Code-credentials-{}", &digest[..8]);
    let args: Value = serde_json::from_slice(
        &std::fs::read(root.join("keychain-cleanup.json"))
            .expect("the temporary keychain item must be cleared"),
    )
    .unwrap();
    assert_eq!(args, json!(["delete-generic-password", "-s", expected]));
    assert!(!root.join("simulated-keychain").exists());
    assert!(root.join("existing-keychain-account").exists());
}
#[cfg(not(target_os = "macos"))]
fn assert_private_keychain_removed(_: &Path, _: &Path) {}

#[cfg(target_os = "macos")]
#[tokio::test]
async fn claude_manual_sign_in_cancel_bounds_a_stalled_keychain_cleanup() {
    let directory = fixture::Directory::new();
    let (mut host, login) = start(&directory.0, "manual", false).await;
    let profile = profile_directory(&directory.0);
    std::fs::write(directory.0.join("stall-keychain"), b"hold").unwrap();
    let id = host
        .send("cancelAuthorization", json!({"session":login["session"]}))
        .await;
    let result = tokio::time::timeout(Duration::from_secs(2), host.frame(id))
        .await
        .unwrap();
    assert_eq!(result["result"]["ok"], true);
    removed(&profile).await;
    assert!(directory.0.join("keychain-cleanup.json").is_file());
    assert!(directory.0.join("existing-keychain-account").exists());
    let pid: i32 = std::fs::read_to_string(directory.0.join("keychain-pid"))
        .unwrap()
        .parse()
        .unwrap();
    assert_eq!(unsafe { libc::kill(pid, 0) }, -1);
    assert!(callback(&mut host, &login["session"], json!({}))
        .await
        .get("error")
        .is_some());
}
