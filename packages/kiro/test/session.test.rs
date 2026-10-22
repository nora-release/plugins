use super::*;
fn temp() -> std::path::PathBuf {
    std::env::temp_dir().join(format!(
        "nora-kiro-test-{}",
        nora_ai::oauth_pkce::random_hex(12)
    ))
}
#[test]
fn source_regions_and_timestamps_are_bounded_and_do_not_accept_endpoints() {
    for input in [
        "https://evil.test",
        "us-east-1/evil",
        "us-east-1@evil",
        "../../us-east-1",
        "us_east_1",
    ] {
        assert!(region(input).is_err());
    }
    assert_eq!(region("eu-central-1").unwrap(), "eu-central-1");
    assert_eq!(timestamp(Some(&json!(1800000000))), 1800000000000);
    assert_eq!(
        timestamp(Some(&json!("2027-01-15T08:00:00Z"))),
        1800000000000
    );
    let cred = Credential {
        region: "eu-west-1".into(),
        ..Default::default()
    };
    assert_eq!(cred.runtime_region().unwrap(), "eu-central-1");
    let cred = Credential {
        profile: "arn:aws:codewhisperer:us-west-2:123:profile/test".into(),
        ..cred
    };
    assert_eq!(cred.runtime_region().unwrap(), "us-west-2");
}
#[test]
fn selected_cli_database_is_read_only_and_uses_the_actual_odic_key() {
    let path = temp();
    let db = rusqlite::Connection::open(&path).unwrap();
    db.execute_batch("CREATE TABLE auth_kv (key TEXT PRIMARY KEY, value TEXT NOT NULL)")
        .unwrap();
    db.execute("INSERT INTO auth_kv VALUES (?1,?2)",["kirocli:odic:token",&json!({"access_token":"cli-private","refresh_token":"cli-refresh","expires_at":"2027-01-15T08:00:00Z","region":"eu-west-1","start_url":"https://company.awsapps.com/start"}).to_string()]).unwrap();
    db.execute(
        "INSERT INTO auth_kv VALUES (?1,?2)",
        [
            "kirocli:odic:device-registration",
            &json!({"client_id":"client","client_secret":"private-secret"}).to_string(),
        ],
    )
    .unwrap();
    drop(db);
    let before = std::fs::read(&path).unwrap();
    let cred = read_source("cli", path.to_str().unwrap()).unwrap();
    assert_eq!(cred.method, "idc");
    assert!(!cred.builder);
    assert_eq!(cred.client_secret, "private-secret");
    assert_eq!(before, std::fs::read(&path).unwrap());
    std::fs::remove_file(path).unwrap();
}
#[test]
fn selected_ide_registration_is_a_related_file_and_cannot_traverse_directories() {
    let directory = temp();
    std::fs::create_dir(&directory).unwrap();
    let path = directory.join("kiro-auth-token.json");
    std::fs::write(
        directory.join("aabb.json"),
        json!({"clientId":"client","clientSecret":"secret"}).to_string(),
    )
    .unwrap();
    std::fs::write(&path,json!({"accessToken":"access","authMethod":"idc","provider":"BuilderId","clientIdHash":"aabb"}).to_string()).unwrap();
    let cred = read_source("ide", path.to_str().unwrap()).unwrap();
    assert_eq!(cred.method, "idc");
    assert!(cred.builder);
    std::fs::write(
        &path,
        json!({"accessToken":"access","clientIdHash":"../aabb"}).to_string(),
    )
    .unwrap();
    assert!(read_source("ide", path.to_str().unwrap()).is_err());
    std::fs::remove_dir_all(directory).unwrap();
}
#[test]
fn missing_or_invalid_sources_never_fall_back_to_another_account() {
    assert!(read_source("cli", "relative.sqlite3").is_err());
    assert!(read_source("unknown", "/dev/null").is_err());
    let path = temp();
    std::fs::write(&path, "private-token invalid JSON").unwrap();
    let error = read_source("ide", path.to_str().unwrap()).err().unwrap();
    assert!(!error.message.contains("private-token"));
    std::fs::remove_file(path).unwrap();
}
#[test]
fn saved_credentials_retain_the_private_renewal_fields_without_debug_output() {
    let mut credential = Credential::api_key(&json!({"apiKey":"ksk_private"})).unwrap();
    assert!(credential.fresh());
    assert_eq!(credential.token_type(), Some("API_KEY"));
    credential.method = "idc".into();
    credential.client_id = "client".into();
    credential.client_secret = "secret".into();
    credential.refresh = "refresh".into();
    credential.expires = now() + 3600000;
    let next = Credential::from_saved(&credential.saved()).unwrap();
    assert_eq!(next.client_secret, "secret");
    assert_eq!(next.refresh, "refresh");
    assert!(next.fresh());
}
#[test]
fn external_idp_cli_source_is_supported_without_reusing_the_company_token_endpoint() {
    let path = temp();
    let db = rusqlite::Connection::open(&path).unwrap();
    db.execute_batch("CREATE TABLE auth_kv (key TEXT PRIMARY KEY,value TEXT NOT NULL)")
        .unwrap();
    db.execute("INSERT INTO auth_kv VALUES (?1,?2)",["kirocli:external-idp:token",&json!({"access_token":"external-private","token_endpoint":"https://company.invalid/token","region":"us-east-1"}).to_string()]).unwrap();
    drop(db);
    let credential = read_source("cli", path.to_str().unwrap()).unwrap();
    assert_eq!(credential.token_type(), Some("EXTERNAL_IDP"));
    std::fs::remove_file(path).unwrap();
}
