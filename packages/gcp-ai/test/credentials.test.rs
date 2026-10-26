use super::*;
use crate::test_support::{Client, Directory};
use std::collections::BTreeMap;

fn google() -> Client {
    Client::new(|request, _| {
        if let Some(reply) = crate::test_support::user_info(request) {
            return reply;
        }
        if request.url.contains("cloudresourcemanager") {
            return (
                200,
                json!({"projects":[{"projectId":"disabled-project","lifecycleState":"ACTIVE"},{"projectId":"fixture-project","lifecycleState":"ACTIVE"}]}),
            );
        }
        if request.url.contains("disabled-project") {
            return (200, json!({"state":"DISABLED"}));
        }
        (200, json!({"state":"ENABLED"}))
    })
}
#[cfg(unix)]
fn browser(directory: &Directory) -> crate::browser::BrowserService {
    use std::os::unix::fs::PermissionsExt;
    let path = directory.0.join("chrome-signin");
    std::fs::write(&path, include_bytes!("fixtures/chrome-signin.sh")).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
    crate::browser::BrowserService::with_signin_executable(path)
}

#[cfg(unix)]
#[tokio::test]
async fn adc_mode_uses_its_own_token_and_never_names_it_from_the_active_gcloud_account() {
    let directory = Directory::new();
    let mut settings = directory.settings();
    settings.auth_mode = "auto".into();
    settings.account = None;
    settings.project = None;
    let path = adc_file(&settings);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(
        &path,
        br#"{"quota_project_id":"unrelated-billing-project"}"#,
    )
    .unwrap();
    let cli = directory.cli(BTreeMap::from([(
        "FAKE_GCLOUD_ACCOUNT".into(),
        "other@example.invalid".into(),
    )]));
    let identity = resolve(&cli, &settings).await.unwrap();
    assert_eq!(identity.auth_mode, "adc");
    assert_eq!(identity.account, None);
    assert_eq!(identity.project.as_deref(), Some("fixture-project"));
    assert_eq!(cli.token(&identity).await.unwrap(), "fixture-adc-token");
    assert!(!directory.commands().contains("get-value account"));
    assert!(!directory
        .commands()
        .contains("auth print-access-token --account"));
}
#[tokio::test]
async fn project_selection_keeps_cloud_resource_manager_order_and_only_enabled_vertex_projects() {
    let directory = Directory::new();
    let mut settings = directory.settings();
    settings.project = None;
    let projects = usable_projects(&google(), "fixture-token").await.unwrap();
    assert_eq!(
        choose_project(&projects, &settings, "person@example.invalid").unwrap(),
        "fixture-project"
    );
    settings.project = Some("disabled-project".into());
    assert!(choose_project(&projects, &settings, "person@example.invalid").is_err());
    settings.project = Some("not-listed-project".into());
    assert_eq!(
        choose_project(&projects, &settings, "person@example.invalid").unwrap(),
        "not-listed-project"
    );
}
#[cfg(unix)]
#[tokio::test]
async fn readable_adc_names_are_disambiguated_when_another_account_owns_the_same_slug() {
    let directory = Directory::new();
    let settings = directory.settings();
    let cli = directory.cli(BTreeMap::new());
    let root = &settings.adc_root;
    let first = plain("a.b@example.invalid", root);
    std::fs::create_dir_all(&first).unwrap();
    std::fs::write(
        first.join("application_default_credentials.json"),
        br#"{"owner":"a.b@example.invalid"}"#,
    )
    .unwrap();
    let second = directory_to_create("a-b@example.invalid", &settings, &cli, &google()).await;
    assert_ne!(first, second);
    assert!(second
        .file_name()
        .unwrap()
        .to_string_lossy()
        .starts_with("adc-a-b-example-invalid-"));
    assert_eq!(
        directory_owned("a.b@example.invalid", &settings, &cli, &google()).await,
        first
    );
    std::fs::create_dir_all(&second).unwrap();
    std::fs::write(
        second.join("application_default_credentials.json"),
        br#"{"owner":"a-b@example.invalid"}"#,
    )
    .unwrap();
    assert_eq!(
        directory_owned("a-b@example.invalid", &settings, &cli, &google()).await,
        second
    );
}
#[test]
fn incomplete_and_pending_directories_do_not_make_an_account_look_signed_in() {
    let directory = Directory::new();
    let settings = directory.settings();
    for name in [
        "adc-person-example-invalid",
        "adc-pending-unknown",
        "adc-empty",
        "other-folder",
    ] {
        let path = settings.adc_root.join(name);
        std::fs::create_dir_all(&path).unwrap();
        if name != "adc-empty" {
            std::fs::write(path.join("application_default_credentials.json"), b"{}").unwrap();
        }
    }
    assert_eq!(known_accounts(&settings), vec!["person-example-invalid"]);
}
#[test]
fn add_account_wraps_the_original_consent_url_once_without_losing_state_or_scope() {
    let consent="https://accounts.google.com/o/oauth2/auth?state=x%26y&scope=a%20b&redirect_uri=http%3A%2F%2Flocalhost%3A8765";
    assert_eq!(sign_in_url(Some(consent), false), consent);
    let added = url::Url::parse(&sign_in_url(Some(consent), true)).unwrap();
    assert_eq!(added.path(), "/AddSession");
    assert_eq!(
        added
            .query_pairs()
            .filter(|(key, _)| key == "continue")
            .count(),
        1
    );
    assert_eq!(
        added
            .query_pairs()
            .find(|(key, _)| key == "continue")
            .unwrap()
            .1,
        consent
    );
}

#[cfg(unix)]
#[tokio::test]
async fn new_signin_uses_undriven_chrome_names_its_adc_and_discovers_an_enabled_project() {
    let directory = Directory::new();
    let mut settings = directory.settings();
    settings.project = None;
    settings.account = None;
    settings.cloudsdk_config = None;
    settings.profile = directory.0.join("profile");
    let cli = directory.cli(BTreeMap::from([(
        "FAKE_LOGIN_READY".into(),
        settings
            .profile
            .join("signin-args")
            .to_string_lossy()
            .into_owned(),
    )]));
    let result = start_login(settings.clone(), &cli, &google(), &browser(&directory))
        .await
        .unwrap();
    let credentials = &result["credentials"];
    assert_eq!(
        credentials["accountIdentity"]["subject"],
        "person@example.invalid"
    );
    assert_eq!(credentials["extras"]["project"], "fixture-project");
    assert_eq!(credentials["access"], "");
    assert_eq!(credentials["refresh"], "");
    let path = PathBuf::from(credentials["extras"]["cloudsdkConfig"].as_str().unwrap());
    assert_eq!(path, plain("person@example.invalid", &settings.adc_root));
    assert!(path.join("application_default_credentials.json").is_file());
    let args = std::fs::read_to_string(settings.profile.join("signin-args")).unwrap();
    assert!(!args.contains("remote-debugging"));
    assert!(!args.contains("enable-automation"));
    assert!(!args.contains("AddSession"));
    assert!(std::fs::read_dir(settings.adc_root)
        .unwrap()
        .all(|entry| !entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with("adc-pending-")));
}
#[cfg(unix)]
#[tokio::test]
async fn a_named_duplicate_opens_no_window_and_a_different_consent_owner_saves_nothing() {
    let directory = Directory::new();
    let mut settings = directory.settings();
    settings.cloudsdk_config = None;
    settings.profile = directory.0.join("profile");
    let existing = plain("person@example.invalid", &settings.adc_root);
    std::fs::create_dir_all(&existing).unwrap();
    std::fs::write(
        existing.join("application_default_credentials.json"),
        br#"{"owner":"person@example.invalid"}"#,
    )
    .unwrap();
    let cli = directory.cli(BTreeMap::new());
    assert!(
        start_login(settings.clone(), &cli, &google(), &browser(&directory))
            .await
            .is_err()
    );
    assert!(!settings.profile.join("signin-args").exists());
    assert!(!directory.commands().contains("application-default login"));
    settings.account = Some("different@example.invalid".into());
    let wrong = plain("different@example.invalid", &settings.adc_root);
    assert!(start_login(settings, &cli, &google(), &browser(&directory))
        .await
        .is_err());
    assert!(!wrong.exists());
    assert!(existing
        .join("application_default_credentials.json")
        .is_file());
}
#[cfg(unix)]
#[tokio::test]
async fn no_vertex_project_and_missing_cli_register_nothing_and_remove_only_new_adc_state() {
    let directory = Directory::new();
    let mut settings = directory.settings();
    settings.cloudsdk_config = None;
    settings.account = None;
    settings.project = None;
    settings.profile = directory.0.join("profile");
    let cli = directory.cli(BTreeMap::new());
    let disabled = Client::new(|request, _| {
        if request.url.contains("userinfo") {
            (200, json!({"email":"person@example.invalid"}))
        } else {
            (200, json!({"projects":[]}))
        }
    });
    assert!(
        start_login(settings.clone(), &cli, &disabled, &browser(&directory))
            .await
            .is_err()
    );
    assert!(std::fs::read_dir(&settings.adc_root)
        .unwrap()
        .next()
        .is_none());
    let missing = Gcloud::new(directory.0.join("missing-gcloud"), BTreeMap::new());
    assert!(
        start_login(settings.clone(), &missing, &google(), &browser(&directory))
            .await
            .is_err()
    );
    assert!(std::fs::read_dir(&settings.adc_root)
        .unwrap()
        .next()
        .is_none());
}
#[cfg(unix)]
#[tokio::test]
async fn explicitly_pinned_existing_adc_is_read_only_and_does_not_open_another_consent_window() {
    let directory = Directory::new();
    let mut settings = directory.settings();
    settings.account = None;
    let file = adc_file(&settings);
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    std::fs::write(
        &file,
        br#"{"owner":"person@example.invalid","keep":"existing"}"#,
    )
    .unwrap();
    let before = std::fs::read(&file).unwrap();
    let cli = directory.cli(BTreeMap::new());
    let result = start_login(settings, &cli, &google(), &browser(&directory))
        .await
        .unwrap();
    assert_eq!(
        result["credentials"]["accountIdentity"]["subject"],
        "person@example.invalid"
    );
    assert_eq!(std::fs::read(file).unwrap(), before);
    assert!(!directory.commands().contains("application-default login"));
    assert_eq!(
        result["credentials"]["extras"]["creditsSession"],
        "unavailable"
    );
}
