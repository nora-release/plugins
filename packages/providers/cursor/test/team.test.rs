use super::*;
use crate::fixture::{Client as Script, Reply};
use serde_json::json;

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!("cursor-team-{}", crate::auth::random()));
        std::fs::create_dir(&dir).unwrap();
        Self(dir)
    }
    fn file(&self) -> PathBuf {
        self.0.join("cli-config.json")
    }
    fn write(&self, value: Value) {
        std::fs::write(self.file(), value.to_string()).unwrap();
    }
    fn teams(&self) -> Teams {
        Teams {
            path: Some(self.file()),
            ..Default::default()
        }
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
fn token(subject: &str) -> String {
    format!(
        "e30.{}.signature",
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(json!({"sub":subject}).to_string())
    )
}
fn configuration(id: Value) -> Value {
    json!({"authInfo":{"authId":"auth0|one","activeTeamId":id}})
}

#[test]
fn config_path_follows_cursor_override_then_xdg_then_home_on_every_platform() {
    let mut env = BTreeMap::from([
        ("HOME".into(), "/home/user".into()),
        ("XDG_CONFIG_HOME".into(), "/config".into()),
        ("CURSOR_CONFIG_DIR".into(), " /selected ".into()),
    ]);
    assert_eq!(
        config_path(&env).unwrap(),
        PathBuf::from("/selected/cli-config.json")
    );
    env.insert("CURSOR_CONFIG_DIR".into(), "  ".into());
    assert_eq!(
        config_path(&env).unwrap(),
        PathBuf::from("/config/cursor/cli-config.json")
    );
    env.remove("XDG_CONFIG_HOME");
    assert_eq!(
        config_path(&env).unwrap(),
        PathBuf::from("/home/user/.cursor/cli-config.json")
    );
}

#[test]
fn active_team_requires_exact_auth_identity_and_a_positive_safe_integer() {
    let fixture = Fixture::new();
    for (id, expected) in [
        (json!(42), 42),
        (json!(42.0), 42),
        (json!(9007199254740991u64), 9007199254740991),
    ] {
        fixture.write(configuration(id.clone()));
        let teams = fixture.teams();
        assert_eq!(teams.active(&token("auth0|one")), Some(expected));
        for token in [
            token("auth0|other"),
            "not-a-jwt".into(),
            token(""),
            "a.invalid-json.sig".into(),
        ] {
            assert_eq!(teams.active(&token), None);
        }
    }
    for id in [
        json!("42"),
        json!(0),
        json!(-1),
        json!(1.5),
        json!(9007199254740992u64),
        json!(true),
        Value::Null,
    ] {
        fixture.write(configuration(id));
        assert_eq!(fixture.teams().active(&token("auth0|one")), None);
    }
    for value in [
        json!({"authInfo":{"authId":"auth0|one","teamId":42}}),
        json!({"authInfo":{"activeTeamId":42}}),
        json!({"authInfo":{"authId":"auth0|one ","activeTeamId":42}}),
        json!({}),
    ] {
        fixture.write(value);
        assert_eq!(fixture.teams().active(&token("auth0|one")), None);
    }
}

#[tokio::test(start_paused = true)]
async fn half_written_configuration_keeps_its_previous_owner_but_deletion_clears_it() {
    let fixture = Fixture::new();
    let teams = fixture.teams();
    assert_eq!(teams.active(&token("auth0|one")), None);
    fixture.write(configuration(json!(42)));
    assert_eq!(teams.active(&token("auth0|one")), None);
    tokio::time::advance(Duration::from_secs(10)).await;
    assert_eq!(teams.active(&token("auth0|one")), Some(42));
    std::fs::write(fixture.file(), "{\"authInfo\":").unwrap();
    tokio::time::advance(Duration::from_secs(10)).await;
    assert_eq!(teams.active(&token("auth0|one")), Some(42));
    assert_eq!(teams.active(&token("auth0|other")), None);
    fixture.write(json!({"authInfo":{"authId":"auth0|other","activeTeamId":99}}));
    tokio::time::advance(Duration::from_secs(10)).await;
    assert_eq!(teams.active(&token("auth0|one")), None);
    assert_eq!(teams.active(&token("auth0|other")), Some(99));
    std::fs::remove_file(fixture.file()).unwrap();
    tokio::time::advance(Duration::from_secs(10)).await;
    assert_eq!(teams.active(&token("auth0|other")), None);
}

#[tokio::test]
async fn a_request_scope_cannot_inherit_a_different_accounts_team_or_override_the_selection() {
    let raw = Script::new(vec![Reply::json(json!({})), Reply::json(json!({}))]);
    let owner = token("auth0|one");
    let client = fixed_client(raw.clone(), &owner, Some(42));
    for token in [&owner, &token("auth0|other")] {
        let request = crate::network::post(
            "https://api2.cursor.sh/fixture",
            b"{}".to_vec(),
            token,
            "cli-test",
            "application/json",
        )
        .with_header("x-cursor-team-id", "injected");
        client.stream(request).await.unwrap();
    }
    let requests = raw.requests.lock().unwrap();
    assert_eq!(requests[0].headers["x-cursor-team-id"], "42");
    assert!(!requests[1].headers.contains_key("x-cursor-team-id"));
}
