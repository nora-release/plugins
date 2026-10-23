use super::*;
#[path = "fixture.rs"]
mod fixture;

struct Directory(PathBuf);
impl Directory {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "nora-kiro-discovery-{}",
            nora_ai::oauth_pkce::random_hex(12)
        ));
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }
    fn sources(&self) -> Vec<Source> {
        vec![
            Source {
                kind: SourceKind::Cli,
                path: self.0.join("data.sqlite3"),
            },
            Source {
                kind: SourceKind::Ide,
                path: self.0.join("kiro-auth-token.json"),
            },
        ]
    }
    fn cli(&self, token: &str, profile: &str, expires: i64) {
        let path = self.0.join("data.sqlite3");
        let db = rusqlite::Connection::open(path).unwrap();
        db.execute_batch(
            "CREATE TABLE IF NOT EXISTS auth_kv(key TEXT PRIMARY KEY,value TEXT NOT NULL)",
        )
        .unwrap();
        db.execute("INSERT OR REPLACE INTO auth_kv VALUES (?1,?2)", ["kirocli:social:token", &json!({"access_token":token,"refresh_token":"private-cli-refresh","expires_at":expires,"region":"eu-central-1","profile_arn":profile}).to_string()]).unwrap();
    }
    fn ide(&self, token: &str, profile: &str, expires: i64) {
        std::fs::write(self.0.join("kiro-auth-token.json"), json!({"accessToken":token,"refreshToken":"private-ide-refresh","expiresAt":expires,"authMethod":"social","profileArn":profile,"region":"eu-central-1"}).to_string()).unwrap();
    }
}
impl Drop for Directory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn provider(client: Arc<fixture::Client>) -> Kiro {
    let mut provider = Kiro::default();
    provider.client = Some(client);
    *provider.config.write().unwrap() = Some(Config {
        proxy: None,
        methods: json!([]),
        installation: "fixture".into(),
    });
    provider
}

#[test]
fn default_paths_match_kiro_cli_and_ide_and_respect_absolute_data_directory_overrides() {
    let home = Path::new("/fixture-home");
    let mac = default_sources(home, &BTreeMap::new(), "macos");
    assert_eq!(
        mac[0].path,
        home.join("Library/Application Support/kiro-cli/data.sqlite3")
    );
    assert_eq!(
        mac[1].path,
        home.join(".aws/sso/cache/kiro-auth-token.json")
    );
    let linux = default_sources(
        home,
        &BTreeMap::from([("XDG_DATA_HOME".into(), "/fixture-data".into())]),
        "linux",
    );
    assert_eq!(
        linux[0].path,
        PathBuf::from("/fixture-data/kiro-cli/data.sqlite3")
    );
    let ignored = default_sources(
        home,
        &BTreeMap::from([("XDG_DATA_HOME".into(), "relative".into())]),
        "linux",
    );
    assert_eq!(
        ignored[0].path,
        home.join(".local/share/kiro-cli/data.sqlite3")
    );
    let windows = default_sources(
        home,
        &BTreeMap::from([("APPDATA".into(), "/fixture-roaming".into())]),
        "windows",
    );
    assert_eq!(
        windows[0].path,
        PathBuf::from("/fixture-roaming/kiro-cli/data.sqlite3")
    );
}

#[tokio::test]
async fn cli_and_ide_report_the_same_verified_identity_without_copying_or_writing_tokens() {
    let directory = Directory::new();
    directory.cli(
        "private-cli-access",
        fixture::PROFILE,
        session::now() + 3_600_000,
    );
    directory.ide(
        "private-ide-access",
        fixture::PROFILE,
        session::now() + 3_600_000,
    );
    let sources = directory.sources();
    let before = sources
        .iter()
        .map(|source| std::fs::read(&source.path).unwrap())
        .collect::<Vec<_>>();
    let client = Arc::new(fixture::Client::new(vec![
        fixture::Reply::json(fixture::usage()),
        fixture::Reply::json(fixture::usage()),
        fixture::Reply::json(fixture::usage()),
    ]));
    let result = discover_at(&sources, client.as_ref()).await.unwrap();
    let accounts = result["accounts"].as_array().unwrap();
    assert_eq!(accounts.len(), 2);
    assert_eq!(
        accounts[0]["credentials"]["accountIdentity"],
        accounts[1]["credentials"]["accountIdentity"]
    );
    assert_eq!(accounts[0]["sourceID"], "cliSource");
    assert_eq!(accounts[1]["sourceID"], "ideSource");
    for (index, account) in accounts.iter().enumerate() {
        assert_eq!(
            account["credentials"]["extras"]["noraAccountSource"]["kind"],
            account["sourceID"]
        );
        assert!(account["credentials"].get("access").is_none());
        assert!(account["credentials"].get("refresh").is_none());
        assert!(!account.to_string().contains("private-"));
        assert_eq!(std::fs::read(&sources[index].path).unwrap(), before[index]);
    }
    assert!(result.get("errors").is_none());
    let owned = provider(client.clone())
        .accounts(CallContext {
            account: "owned".into(),
            credentials: Some(fixture::saved()),
            network_proxy: None,
        })
        .await
        .unwrap();
    assert_eq!(
        owned["credentials"]["accountIdentity"],
        accounts[0]["credentials"]["accountIdentity"]
    );
    assert_eq!(client.requests.lock().unwrap().len(), 3);
}

#[tokio::test]
async fn cli_and_owned_magpie_login_share_the_verified_user_identity_and_return_distinct_token_free_refs(
) {
    let directory = Directory::new();
    directory.cli(
        "private-cli-access",
        fixture::PROFILE,
        session::now() + 3_600_000,
    );
    let magpie_path = directory.0.join("plugin-auth.json");
    let original=json!({"kiro#owned":{"type":"oauth","access":"private-magpie-access","refresh":"private-magpie-refresh","expires":session::now()+3_600_000,"method":"social","region":"eu-central-1","profileArn":fixture::PROFILE,"accountId":"person@example.test (Google)","uid":"google","loginProvider":"Google"},"kiro":{"type":"oauth","source":"kiro-cli","access":"","refresh":""}}).to_string();
    std::fs::write(&magpie_path, &original).unwrap();
    let sources = vec![
        directory.sources().remove(0),
        Source {
            kind: SourceKind::Magpie(crate::source_kind::MagpieAccount::new("kiro#owned").unwrap()),
            path: magpie_path.clone(),
        },
    ];
    let mut usage = fixture::usage();
    usage["userInfo"]["userId"] = json!("verified-same-user");
    let client = fixture::Client::new(vec![
        fixture::Reply::json(usage.clone()),
        fixture::Reply::json(usage),
    ]);
    let result = discover_at(&sources, &client).await.unwrap();
    assert!(result.get("errors").is_none());
    let accounts = result["accounts"].as_array().unwrap();
    assert_eq!(accounts.len(), 2);
    assert_eq!(
        accounts[0]["credentials"]["accountIdentity"],
        accounts[1]["credentials"]["accountIdentity"]
    );
    assert_eq!(accounts[0]["sourceID"], "cliSource");
    assert_eq!(accounts[1]["sourceID"], "magpieSource");
    assert_eq!(
        accounts[1]["credentials"]["source"],
        json!({"kind":"magpie","account":"kiro#owned","path":magpie_path})
    );
    assert_eq!(
        accounts[1]["credentials"]["extras"]["noraAccountSource"]["account"],
        "kiro#owned"
    );
    for account in accounts {
        assert!(account["credentials"].get("access").is_none());
        assert!(account["credentials"].get("refresh").is_none());
    }
    assert!(!result.to_string().contains("private-"));
    assert_eq!(std::fs::read_to_string(&magpie_path).unwrap(), original);
    assert_eq!(client.requests.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn detected_magpie_login_is_reported_when_identity_service_is_temporarily_unavailable() {
    let directory = Directory::new();
    let path = directory.0.join("plugin-auth.json");
    let original=json!({"kiro#owned":{"type":"oauth","access":"private-magpie-access","refresh":"private-magpie-refresh","expires":session::now()+3_600_000,"method":"social","profileArn":fixture::PROFILE}}).to_string();
    std::fs::write(&path, &original).unwrap();
    let client = fixture::Client::new(vec![fixture::Reply::raw(
        503,
        b"private-service-response".to_vec(),
    )]);
    let result = discover_at(
        &[Source {
            kind: SourceKind::Magpie(crate::source_kind::MagpieAccount::new("kiro#owned").unwrap()),
            path: path.clone(),
        }],
        &client,
    )
    .await
    .unwrap();
    assert_eq!(result["accounts"], json!([]));
    assert_eq!(result["errors"][0]["sourceID"], "magpieSource");
    assert_eq!(result["errors"][0]["detected"], true);
    assert!(!result.to_string().contains("private-"));
    assert_eq!(std::fs::read_to_string(path).unwrap(), original);
}

#[tokio::test]
async fn different_profiles_are_not_merged_even_when_the_service_returns_the_same_email() {
    let directory = Directory::new();
    directory.cli(
        "first-session",
        fixture::PROFILE,
        session::now() + 3_600_000,
    );
    directory.ide(
        "second-session",
        "arn:aws:codewhisperer:eu-central-1:123456789012:profile/other",
        session::now() + 3_600_000,
    );
    let client = fixture::Client::new(vec![
        fixture::Reply::json(fixture::usage()),
        fixture::Reply::json(fixture::usage()),
    ]);
    let result = discover_at(&directory.sources(), &client).await.unwrap();
    assert_ne!(
        result["accounts"][0]["credentials"]["accountIdentity"],
        result["accounts"][1]["credentials"]["accountIdentity"]
    );
}

#[tokio::test]
async fn absent_sources_are_empty_but_unreadable_sources_preserve_state_and_do_not_hide_a_valid_other_source(
) {
    let directory = Directory::new();
    let client = fixture::Client::new(vec![]);
    assert_eq!(
        discover_at(&directory.sources(), &client).await.unwrap(),
        json!({"accounts":[]})
    );
    std::fs::write(directory.0.join("data.sqlite3"), "not a database").unwrap();
    directory.ide("ide-session", fixture::PROFILE, session::now() + 3_600_000);
    let client = fixture::Client::new(vec![fixture::Reply::json(fixture::usage())]);
    let result = discover_at(&directory.sources(), &client).await.unwrap();
    assert_eq!(result["accounts"].as_array().unwrap().len(), 1);
    assert_eq!(result["accounts"][0]["sourceID"], "ideSource");
    assert_eq!(result["errors"].as_array().unwrap().len(), 1);
    assert_eq!(result["errors"][0]["sourceID"], "cliSource");
    assert!(!result.to_string().contains("ide-session"));
}

#[tokio::test]
async fn failed_expired_source_renewal_preserves_sources_and_reports_errors_without_copying_tokens()
{
    let directory = Directory::new();
    directory.cli("expired-cli-access", fixture::PROFILE, session::now() - 1);
    directory.ide("expired-ide-access", fixture::PROFILE, session::now() - 1);
    let sources = directory.sources();
    let before = sources
        .iter()
        .map(|source| std::fs::read(&source.path).unwrap())
        .collect::<Vec<_>>();
    let client = fixture::Client::new(vec![
        fixture::Reply::raw(500, b"unavailable".to_vec()),
        fixture::Reply::raw(500, b"unavailable".to_vec()),
    ]);
    let result = discover_at(&sources, &client).await.unwrap();
    assert!(result["accounts"].as_array().unwrap().is_empty());
    assert_eq!(result["errors"].as_array().unwrap().len(), 2);
    assert_eq!(client.requests.lock().unwrap().len(), 2);
    for (index, source) in sources.iter().enumerate() {
        assert_eq!(std::fs::read(&source.path).unwrap(), before[index]);
    }
}

#[tokio::test]
async fn a_source_switch_never_serves_the_new_profile_under_the_old_account() {
    let directory = Directory::new();
    directory.ide(
        "first-session",
        fixture::PROFILE,
        session::now() + 3_600_000,
    );
    let sources = vec![directory.sources().remove(1)];
    let client = Arc::new(fixture::Client::new(vec![
        fixture::Reply::json(fixture::usage()),
        fixture::Reply::json(fixture::usage()),
    ]));
    let first =
        discover_at(&sources, client.as_ref()).await.unwrap()["accounts"][0]["credentials"].clone();
    directory.ide(
        "second-session",
        "arn:aws:codewhisperer:eu-central-1:123456789012:profile/other",
        session::now() + 3_600_000,
    );
    let provider = provider(client.clone());
    assert!(provider
        .live(Some(&first), client.as_ref(), false)
        .await
        .is_err());
    assert_eq!(client.requests.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn legacy_owned_profiles_gain_the_same_identity_as_a_verified_source_without_a_new_sign_in() {
    let client = Arc::new(fixture::Client::new(vec![fixture::Reply::json(
        fixture::usage(),
    )]));
    let provider = provider(client.clone());
    let saved = fixture::saved();
    let result = provider
        .accounts(CallContext {
            account: "legacy".into(),
            credentials: Some(saved),
            network_proxy: None,
        })
        .await
        .unwrap();
    assert!(result["credentials"]["accountIdentity"].is_object());
    assert_eq!(result["accounts"][0]["email"], "person@example.test");
    assert!(!result["accounts"].to_string().contains("private-access"));
    assert!(!result["accounts"].to_string().contains("private-refresh"));
    assert_eq!(client.requests.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn changing_the_source_realm_invalidates_the_verified_cache_even_with_the_same_access_and_profile(
) {
    let directory = Directory::new();
    directory.ide("shared-token", fixture::PROFILE, session::now() + 3_600_000);
    let path = directory.0.join("kiro-auth-token.json");
    std::fs::write(
        directory.0.join("aabb.json"),
        json!({"clientId":"client","clientSecret":"private-client-secret"}).to_string(),
    )
    .unwrap();
    let mut source: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    source["authMethod"] = json!("idc");
    source["clientIdHash"] = json!("aabb");
    source["startUrl"] = json!("https://company-a.awsapps.com/start");
    std::fs::write(&path, source.to_string()).unwrap();
    let sources = vec![directory.sources().remove(1)];
    let client = Arc::new(fixture::Client::new(vec![
        fixture::Reply::json(fixture::usage()),
        fixture::Reply::json(fixture::usage()),
        fixture::Reply::json(fixture::usage()),
    ]));
    let saved =
        discover_at(&sources, client.as_ref()).await.unwrap()["accounts"][0]["credentials"].clone();
    let provider = provider(client.clone());
    provider
        .live(Some(&saved), client.as_ref(), false)
        .await
        .unwrap();
    source["startUrl"] = json!("https://company-b.awsapps.com/start");
    std::fs::write(&path, source.to_string()).unwrap();
    assert!(provider
        .live(Some(&saved), client.as_ref(), false)
        .await
        .is_err());
    assert_eq!(client.requests.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn legacy_owned_account_inspection_does_not_replace_a_saved_person_with_an_upstream_different_one(
) {
    let mut changed = fixture::usage();
    changed["userInfo"]["email"] = json!("different@example.test");
    let client = Arc::new(fixture::Client::new(vec![fixture::Reply::json(changed)]));
    let provider = provider(client.clone());
    let saved = fixture::saved();
    let original = saved.clone();
    assert!(provider
        .accounts(CallContext {
            account: "legacy".into(),
            credentials: Some(saved),
            network_proxy: None
        })
        .await
        .is_err());
    assert_eq!(original["email"], "person@example.test");
    assert_eq!(client.requests.lock().unwrap().len(), 1);
}
