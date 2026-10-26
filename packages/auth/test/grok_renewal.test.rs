use super::*;

#[tokio::test]
async fn torn_auth_file_is_read_again_before_the_account_is_resolved() {
    let fixture = Fixture::new();
    let path = fixture.source.auth_file_path.clone();
    std::fs::write(&path, "{\"partial").unwrap();
    let complete = fixture.auth("restored-token", "account-a", now_millis() + 3_600_000);
    let writer = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(150)).await;
        std::fs::write(path, complete).unwrap();
    });
    let resolved = resolve_credentials(&fixture.credentials(), false).await;
    writer.await.unwrap();
    assert_eq!(resolved.unwrap().access_token, "restored-token");
}

#[tokio::test]
async fn a_torn_file_can_use_only_the_last_complete_record_for_that_source() {
    let fixture = Fixture::new();
    fixture.write("kept-token", "account-a", now_millis() + 3_600_000);
    let credentials = fixture.credentials();
    assert_eq!(
        resolve_credentials(&credentials, false)
            .await
            .unwrap()
            .access_token,
        "kept-token"
    );
    for partial in ["", "{\"partial"] {
        std::fs::write(&fixture.source.auth_file_path, partial).unwrap();
        assert_eq!(
            resolve_credentials(&credentials, false)
                .await
                .unwrap()
                .access_token,
            "kept-token"
        );
    }
    assert!(credentials.access_token.is_empty());
    assert!(credentials.refresh_token.is_none());
    let mut other = fixture.source.clone();
    other.auth_file_path = fixture.directory.join("different-auth.json");
    std::fs::write(&other.auth_file_path, "{").unwrap();
    let mut other_credentials = credentials.clone();
    other_credentials
        .extras
        .insert(METADATA_KEY.into(), json!(other).into());
    let error = resolve_credentials(&other_credentials, false)
        .await
        .err()
        .unwrap();
    assert_eq!(error, SourceError::ReadFailed);
}

#[tokio::test]
async fn unreadable_or_torn_sources_without_history_are_unknown_not_signed_out() {
    let fixture = Fixture::new();
    for partial in ["", "{\"token"] {
        std::fs::write(&fixture.source.auth_file_path, partial).unwrap();
        let error = resolve_credentials(&fixture.credentials(), false)
            .await
            .err()
            .unwrap();
        assert_eq!(error, SourceError::ReadFailed);
    }
    std::fs::remove_file(&fixture.source.auth_file_path).unwrap();
    // Opening a directory succeeds on Unix, then read_to_end fails. That is
    // different from a complete auth file saying the user signed out.
    std::fs::create_dir(&fixture.source.auth_file_path).unwrap();
    let error = resolve_credentials(&fixture.credentials(), false)
        .await
        .err()
        .unwrap();
    assert_eq!(error, SourceError::ReadFailed);
}

#[tokio::test]
async fn complete_logout_or_invalid_credentials_prevent_stale_token_fallback() {
    let fixture = Fixture::new();
    let credentials = fixture.credentials();
    for complete in [
        json!({}),
        json!({SCOPE:{"key":""}}),
        json!({SCOPE:{"key":"foreign","user_id":"account-a","oidc_issuer":"https://wrong.invalid"}}),
    ] {
        fixture.write("old-token", "account-a", now_millis() + 3_600_000);
        resolve_credentials(&credentials, false).await.unwrap();
        std::fs::write(&fixture.source.auth_file_path, complete.to_string()).unwrap();
        assert!(resolve_credentials(&credentials, false).await.is_err());
        std::fs::write(&fixture.source.auth_file_path, "").unwrap();
        let error = resolve_credentials(&credentials, false)
            .await
            .err()
            .unwrap();
        assert_eq!(error, SourceError::ReadFailed);
    }
    fixture.write("old-token", "account-a", now_millis() + 3_600_000);
    resolve_credentials(&credentials, false).await.unwrap();
    std::fs::remove_file(&fixture.source.auth_file_path).unwrap();
    assert_eq!(
        resolve_credentials(&credentials, false).await.err(),
        Some(SourceError::NotSignedIn)
    );
    std::fs::write(&fixture.source.auth_file_path, "").unwrap();
    let error = resolve_credentials(&credentials, false)
        .await
        .err()
        .unwrap();
    assert_eq!(error, SourceError::ReadFailed);
}

#[tokio::test]
async fn a_new_account_never_falls_back_to_the_previous_account_on_a_torn_read() {
    let fixture = Fixture::new();
    let credentials = fixture.credentials();
    fixture.write("old-token", "account-a", now_millis() + 3_600_000);
    resolve_credentials(&credentials, false).await.unwrap();
    fixture.write("other-token", "account-b", now_millis() + 3_600_000);
    assert_eq!(
        resolve_credentials(&credentials, false).await.err(),
        Some(SourceError::AccountChanged)
    );
    std::fs::write(&fixture.source.auth_file_path, "").unwrap();
    assert_eq!(
        resolve_credentials(&credentials, false).await.err(),
        Some(SourceError::AccountChanged)
    );
}

#[cfg(unix)]
fn failing_cli(fixture: &mut Fixture, script: &str) {
    use std::os::unix::fs::PermissionsExt;
    let executable = fixture.directory.join("grok");
    std::fs::write(&executable, format!("#!/bin/sh\nif [ \"$1\" = --version ]; then printf 'grok 99.0.0'; exit 0; fi\n{script}\n")).unwrap();
    std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
    fixture.source.executable_path = Some(executable);
}

#[cfg(unix)]
#[tokio::test]
async fn a_failed_early_renewal_keeps_the_still_valid_token() {
    let mut fixture = Fixture::new();
    failing_cli(&mut fixture, "exit 1");
    fixture.write("valid-token", "account-a", now_millis() + 120_000);
    assert_eq!(
        resolve_credentials(&fixture.credentials(), false)
            .await
            .unwrap()
            .access_token,
        "valid-token"
    );
    assert_eq!(
        resolve_credentials(&fixture.credentials(), true)
            .await
            .err(),
        Some(SourceError::RefreshFailed)
    );
}

#[cfg(unix)]
#[tokio::test]
async fn a_forced_renewal_that_leaves_the_same_token_does_not_report_success() {
    let mut fixture = Fixture::new();
    failing_cli(&mut fixture, "exit 0");
    fixture.write("rejected-token", "account-a", now_millis() + 3_600_000);
    let error = resolve_credentials(&fixture.credentials(), true)
        .await
        .err()
        .expect("The rejected token was not renewed");
    assert_eq!(error, SourceError::RenewalUnchanged);
}

#[cfg(unix)]
#[tokio::test]
async fn a_cli_failure_after_replacing_the_token_uses_the_complete_replacement() {
    let mut fixture = Fixture::new();
    fixture.cli();
    let path = fixture.source.executable_path.as_ref().unwrap();
    let script = std::fs::read_to_string(path).unwrap();
    std::fs::write(path, format!("{script}\nexit 1\n")).unwrap();
    fixture.write("expired-token", "account-a", now_millis() - 1);
    assert_eq!(
        resolve_credentials(&fixture.credentials(), true)
            .await
            .unwrap()
            .access_token,
        "fresh-token"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn renewal_cannot_reuse_an_expired_cached_record_and_timeout_is_not_logout() {
    let mut fixture = Fixture::new();
    failing_cli(&mut fixture, "exit 0");
    fixture.write("expired-token", "account-a", now_millis() - 1);
    assert_eq!(
        resolve_credentials(&fixture.credentials(), false)
            .await
            .err(),
        Some(SourceError::Expired)
    );
    std::fs::write(&fixture.source.auth_file_path, "").unwrap();
    assert_eq!(
        resolve_credentials(&fixture.credentials(), false)
            .await
            .err(),
        Some(SourceError::Expired)
    );
    failing_cli(&mut fixture, "exec /bin/sleep 2");
    assert_eq!(
        fixture
            .source
            .command(&["models"], Duration::from_millis(20), false, None)
            .await
            .err(),
        Some(SourceError::TimedOut)
    );
}
