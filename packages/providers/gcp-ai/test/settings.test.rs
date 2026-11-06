use super::*;
use serde_json::json;

#[test]
fn each_accounts_settings_win_over_the_row_and_the_environment() {
    let env = BTreeMap::from([
        ("HOME".into(), "/fixture/home".into()),
        ("GOOGLE_VERTEX_PROJECT".into(), "environment-project".into()),
        ("GOOGLE_CLOUD_PROJECT".into(), "second-env-project".into()),
        ("GOOGLE_VERTEX_LOCATION".into(), "us-central1".into()),
        (
            "VERTEX_GCLOUD_ACCOUNT".into(),
            "environment@example.invalid".into(),
        ),
        ("CLOUDSDK_CONFIG".into(), "/foreign/machine/config".into()),
    ]);
    let row = json!({"project":"row-project","location":"europe-west1","account":"row@example.invalid","profile":"~/row-profile"});
    let auth = json!({"extras":{"project":"account-project","accountId":"saved@example.invalid","location":"asia-east1","profile":"~/saved-profile","cloudsdkConfig":"~/owned-adc"}});
    let settings =
        Settings::resolve_with_env(&row, Some(&auth), Path::new("/fixture/data"), &env).unwrap();
    assert_eq!(settings.project.as_deref(), Some("account-project"));
    assert_eq!(settings.location, "asia-east1");
    assert_eq!(settings.account.as_deref(), Some("saved@example.invalid"));
    assert_eq!(settings.profile, Path::new("/fixture/home/saved-profile"));
    assert_eq!(
        settings.cloudsdk_config,
        Some(PathBuf::from("/fixture/home/owned-adc"))
    );
    let row = Settings::resolve_with_env(&row, None, Path::new("/fixture/data"), &env).unwrap();
    assert_eq!(row.project.as_deref(), Some("row-project"));
    assert_eq!(row.cloudsdk_config, None);
    let empty = Settings::resolve_with_env(
        &json!({}),
        Some(&json!({"project":""})),
        Path::new("/fixture/data"),
        &env,
    )
    .unwrap();
    assert_eq!(empty.project.as_deref(), Some("environment-project"));
    assert_eq!(empty.cloudsdk_config, None);
}
#[test]
fn account_email_and_project_are_not_borrowed_from_other_row_fields() {
    let env = BTreeMap::from([("HOME".into(), "/fixture/home".into())]);
    let settings = Settings::resolve_with_env(
        &json!({"account":"row@example.invalid","accountId":"never-a-row-owner@example.invalid"}),
        Some(&json!({"account":"account@example.invalid"})),
        Path::new("/fixture/data"),
        &env,
    )
    .unwrap();
    assert_eq!(settings.account.as_deref(), Some("account@example.invalid"));
}
#[test]
fn addresses_and_option_values_are_checked_before_cli_or_network() {
    for project in [
        "Abcdef",
        "short",
        "project with spaces",
        "-project",
        "project-",
        "project/escape",
    ] {
        assert!(validate_project(project).is_err());
    }
    for account in [
        "",
        "person",
        "person@",
        "@domain",
        "-person@example.invalid",
        "other@example.invalid\n--flag",
    ] {
        assert!(validate_account(account).is_err());
    }
    for billing in ["", "123456-abcdef", "123456-xxxxxx-123456"] {
        assert!(validate_billing(billing).is_err());
    }
    let env = BTreeMap::from([("HOME".into(), "/fixture/home".into())]);
    for row in [
        json!({"authMode":"token"}),
        json!({"location":"global/../evil"}),
        json!({"location":"us--central1"}),
    ] {
        assert!(Settings::resolve_with_env(&row, None, Path::new("/fixture/data"), &env).is_err());
    }
    let global = Settings::resolve_with_env(
        &json!({"project":"fixture-project"}),
        None,
        Path::new("/fixture/data"),
        &env,
    )
    .unwrap();
    assert_eq!(global.endpoint("fixture-project").unwrap(),"https://aiplatform.googleapis.com/v1beta1/projects/fixture-project/locations/global/publishers/google");
    let region = Settings::resolve_with_env(
        &json!({"project":"fixture-project","location":"europe-west1"}),
        None,
        Path::new("/fixture/data"),
        &env,
    )
    .unwrap();
    assert!(region
        .endpoint("fixture-project")
        .unwrap()
        .starts_with("https://europe-west1-aiplatform.googleapis.com/"));
}
#[test]
fn invalid_cache_ttls_never_turn_a_read_into_an_unbounded_loop() {
    for value in ["NaN", "inf", "-1", "invalid"] {
        let env = BTreeMap::from([
            ("HOME".into(), "/fixture/home".into()),
            ("VERTEX_MODELS_CACHE_TTL".into(), value.into()),
        ]);
        assert!(
            Settings::resolve_with_env(&json!({}), None, Path::new("/fixture/data"), &env).is_err()
        );
    }
}
