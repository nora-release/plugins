use super::*;
use crate::test_support::Directory;
use serde_json::json;

#[cfg(unix)]
#[tokio::test]
async fn concurrent_tokens_share_one_subprocess_and_the_next_read_checks_expiry_again() {
    let directory = Directory::new();
    let settings = directory.settings();
    let cli = directory.cli(BTreeMap::new());
    let tokens = futures::future::join_all((0..20).map(|_| cli.token(&settings))).await;
    assert!(tokens
        .iter()
        .all(|token| token.as_deref() == Ok("fixture-adc-token")));
    assert_eq!(directory.commands().lines().count(), 1);
    assert_eq!(cli.token(&settings).await.unwrap(), "fixture-adc-token");
    assert_eq!(directory.commands().lines().count(), 2);
}
#[cfg(unix)]
#[tokio::test]
async fn cli_pins_adc_directories_and_never_inherits_the_machine_cloudsdk_override() {
    let directory = Directory::new();
    let mut settings = directory.settings();
    let cli = directory.cli(BTreeMap::from([(
        "CLOUDSDK_CONFIG".into(),
        "/foreign/machine".into(),
    )]));
    cli.token(&settings).await.unwrap();
    assert!(directory.commands().contains(&format!(
        "config={}",
        settings.cloudsdk_config.as_ref().unwrap().display()
    )));
    settings.cloudsdk_config = None;
    settings.auth_mode = "gcloud".into();
    cli.token(&settings).await.unwrap();
    let commands = directory.commands();
    assert!(!commands.contains("/foreign/machine"));
    assert!(commands
        .contains("auth print-access-token --account=person@example.invalid --quiet | config= |"));
}
#[cfg(unix)]
#[tokio::test]
async fn failures_do_not_expose_stdout_stderr_or_token_material() {
    let directory = Directory::new();
    let settings = directory.settings();
    let cli = directory.cli(BTreeMap::from([("FAKE_GCLOUD_FAIL".into(), "yes".into())]));
    let error = cli.token(&settings).await.unwrap_err().to_string();
    assert!(!error.contains("a-secret"));
    assert!(error.contains("gcloud auth application-default login"));
    let cli = directory.cli(BTreeMap::from([(
        "FAKE_GCLOUD_TOKEN".into(),
        "bad token".into(),
    )]));
    assert!(cli.token(&settings).await.is_err());
}
#[cfg(unix)]
#[tokio::test]
async fn consent_keeps_the_complete_url_and_accepts_saved_adc_even_if_quota_setup_failed() {
    let directory = Directory::new();
    let settings = directory.settings();
    let cli = directory.cli(BTreeMap::from([("FAKE_LOGIN_EXIT".into(), "1".into())]));
    let mut consent = cli.begin_adc(&settings).await.unwrap();
    assert!(consent.created_file());
    assert!(consent.url.ends_with(
        "&redirect_uri=http%3A%2F%2Flocalhost%3A8765%2Fcallback&scope=scope-a%20scope-b"
    ));
    consent.finish().await.unwrap();
    assert!(consent.file.is_file());
    assert!(directory.commands().contains("browser=true"));
    let cli = directory.cli(BTreeMap::from([
        ("FAKE_LOGIN_EXIT".into(), "1".into()),
        ("FAKE_LOGIN_NO_ADC".into(), "yes".into()),
    ]));
    std::fs::remove_file(&consent.file).unwrap();
    let mut failed = cli.begin_adc(&settings).await.unwrap();
    assert!(failed.finish().await.is_err());
}
#[cfg(unix)]
#[tokio::test]
async fn cancelling_a_consent_writer_stops_it_before_rollback_and_does_not_recreate_adc() {
    let directory = Directory::new();
    let settings = directory.settings();
    let wait = directory.0.join("wait");
    std::fs::write(&wait, b"wait").unwrap();
    let cli = directory.cli(BTreeMap::from([(
        "FAKE_LOGIN_WAIT".into(),
        wait.to_string_lossy().into_owned(),
    )]));
    let mut consent = cli.begin_adc(&settings).await.unwrap();
    consent.cancel().await;
    std::fs::remove_file(wait).unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(!consent.file.exists());
}
#[test]
fn consent_scanning_requires_a_complete_whitespace_terminated_google_url() {
    let (sender, mut receiver) = tokio::sync::oneshot::channel();
    let mut scanner = Scanner {
        bytes: Vec::new(),
        sender: Some(sender),
    };
    scanner.scan(b"https://accounts.google.com/o/oauth2/auth?state=opaque");
    assert!(receiver.try_recv().is_err());
    scanner.scan(b"&scope=scope-a%20scope-b\n");
    assert_eq!(
        receiver.try_recv().unwrap(),
        "https://accounts.google.com/o/oauth2/auth?state=opaque&scope=scope-a%20scope-b"
    );
    let _ = json!({});
}

#[cfg(unix)]
#[tokio::test]
async fn replacing_adc_cannot_reuse_a_cached_owner_and_refreshing_the_same_person_is_reverified() {
    let directory = Directory::new();
    let settings = directory.settings();
    let cli = directory.cli(BTreeMap::new());
    let file = crate::credentials::adc_file(&settings);
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    let client = crate::test_support::Client::new(|request, _| {
        crate::test_support::user_info(request).unwrap()
    });
    std::fs::write(
        &file,
        br#"{"owner":"person@example.invalid","nonce":"first"}"#,
    )
    .unwrap();
    let first = cli.token(&settings).await.unwrap();
    cli.verify_owner(&settings, &first, &client).await.unwrap();
    cli.verify_owner(&settings, &first, &client).await.unwrap();
    assert_eq!(client.calls.lock().unwrap().len(), 1);
    std::fs::write(
        &file,
        br#"{"owner":"person@example.invalid","nonce":"renewed"}"#,
    )
    .unwrap();
    let renewed = cli.token(&settings).await.unwrap();
    assert_ne!(first, renewed);
    cli.verify_owner(&settings, &renewed, &client)
        .await
        .unwrap();
    assert_eq!(client.calls.lock().unwrap().len(), 2);
    std::fs::write(
        &file,
        br#"{"owner":"different@example.invalid","nonce":"other-person"}"#,
    )
    .unwrap();
    let before = std::fs::read(&file).unwrap();
    let replaced = cli.token(&settings).await.unwrap();
    assert_eq!(
        cli.verify_owner(&settings, &replaced, &client)
            .await
            .unwrap_err()
            .code,
        "authentication_required"
    );
    assert_eq!(std::fs::read(&file).unwrap(), before);
    let mut other = settings.clone();
    other.account = Some("different@example.invalid".into());
    assert!(cli.verify_owner(&other, &first, &client).await.is_err());
    assert_eq!(directory.commands().lines().count(), 3);
}

#[tokio::test]
async fn temporary_identity_failures_are_retried_without_clearing_or_claiming_a_signed_out_account()
{
    let directory = Directory::new();
    let settings = directory.settings();
    let cli = Gcloud::new(directory.0.join("unused-gcloud"), BTreeMap::new());
    let client = crate::test_support::Client::new(|_, index| {
        if index == 0 {
            (503, json!({"secret":"must-not-escape"}))
        } else {
            (200, json!({"email":"person@example.invalid"}))
        }
    });
    let error = cli
        .verify_owner(&settings, "already-printed-token", &client)
        .await
        .unwrap_err();
    assert_eq!(error.code, "upstream_transport_error");
    assert!(!error.to_string().contains("secret"));
    assert!(error.to_string().contains("Credentials were kept"));
    cli.verify_owner(&settings, "already-printed-token", &client)
        .await
        .unwrap();
    cli.verify_owner(&settings, "already-printed-token", &client)
        .await
        .unwrap();
    assert_eq!(client.calls.lock().unwrap().len(), 2);
    assert!(directory.commands().is_empty());
    let mut subject = settings.clone();
    subject.expected_subject = Some("google-subject-one".into());
    let claims = crate::test_support::Client::new(|_, _| {
        (
            200,
            json!({"email":"person@example.invalid","sub":"google-subject-two"}),
        )
    });
    assert_eq!(
        cli.verify_owner(&subject, "already-printed-token", &claims)
            .await
            .unwrap_err()
            .code,
        "authentication_required"
    );
}
