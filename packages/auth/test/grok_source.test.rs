use super::*;
use serde_json::json;
#[path = "grok_renewal.test.rs"]
mod renewal_tests;

#[tokio::test]
async fn binding_keeps_a_source_reference_instead_of_copying_its_token() {
    let fixture = Fixture::new();
    fixture.write(
        "private-local-session",
        "account-a",
        now_millis() + 3_600_000,
    );
    let credentials = fixture.source.bound_credentials().await.unwrap();
    assert!(credentials.access_token.is_empty());
    assert!(credentials.refresh_token.is_none());
    assert!(is_bound(&credentials));
    assert_eq!(
        credentials.extras[ACCOUNT_ID_KEY].as_str(),
        Some("account-a")
    );
    assert!(!serde_json::to_string(&credentials)
        .unwrap()
        .contains("private-local-session"));
}

#[test]
fn source_discovery_respects_explicit_paths_without_using_a_different_grok_installation() {
    let fixture = Fixture::new();
    let environment = BTreeMap::from([
        (
            "GROK_HOME".into(),
            fixture.source.home_directory.to_string_lossy().into_owned(),
        ),
        (
            "GROK_AUTH_PATH".into(),
            fixture.source.auth_file_path.to_string_lossy().into_owned(),
        ),
        ("PATH".into(), "/usr/bin:/bin".into()),
    ]);
    let source = Source::current(&fixture.directory, &environment);
    assert_eq!(source.home_directory, fixture.source.home_directory);
    assert_eq!(source.auth_file_path, fixture.source.auth_file_path);
    assert!(source.executable_path.is_none());
}

struct Fixture {
    directory: PathBuf,
    source: Source,
}

impl Fixture {
    fn new() -> Self {
        use std::sync::atomic::{AtomicU64, Ordering};
        static NEXT_FIXTURE: AtomicU64 = AtomicU64::new(0);
        let directory = std::env::temp_dir().join(format!(
            "nora-grok-cli-source-{}-{}-{}",
            std::process::id(),
            NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let home = directory.join("grok-home");
        std::fs::create_dir_all(&home).unwrap();
        Self {
            source: Source {
                kind: "grokCLI".into(),
                home_directory: home,
                auth_file_path: directory.join("separate-auth.json"),
                executable_path: None,
            },
            directory,
        }
    }

    fn metadata(&self) -> JsonValue {
        json!({"kind":"grokCLI", "homeDirectory":self.source.home_directory, "authFilePath":self.source.auth_file_path, "executablePath":self.source.executable_path}).into()
    }

    fn credentials(&self) -> OAuthCredentials {
        OAuthCredentials::new(
            OAuthProviderId::XAI,
            "",
            None,
            None,
            BTreeMap::from([
                (METADATA_KEY.into(), self.metadata()),
                (ACCOUNT_ID_KEY.into(), JsonValue::string("account-a")),
                ("userId".into(), JsonValue::string("account-a")),
            ]),
        )
    }

    fn auth(&self, token: &str, user: &str, expires: i64) -> Vec<u8> {
        serde_json::to_vec(&json!({SCOPE:{"key":token,"user_id":user,"email":null,"oidc_issuer":ISSUER,"expires_at":chrono::DateTime::from_timestamp_millis(expires).unwrap().to_rfc3339()}})).unwrap()
    }

    fn write(&self, token: &str, user: &str, expires: i64) {
        std::fs::write(&self.source.auth_file_path, self.auth(token, user, expires)).unwrap();
    }

    #[cfg(unix)]
    fn cli(&mut self) {
        use std::os::unix::fs::PermissionsExt;
        let executable = self.directory.join("grok");
        let replacement = self.directory.join("replacement.json");
        std::fs::write(
            &replacement,
            self.auth("fresh-token", "account-a", now_millis() + 3_600_000),
        )
        .unwrap();
        let quote = |path: &PathBuf| format!("'{}'", path.to_string_lossy().replace('\'', "'\\''"));
        let script = format!("#!/bin/sh\nif [ \"$1\" = --version ]; then printf 'grok 99.0.0 (fixture)\\n'; exit 0; fi\n[ \"$GROK_HOME\" = {} ] || exit 1\n[ \"$GROK_AUTH_PATH\" = {} ] || exit 1\n[ -z \"${{GROK_AUTH_PROVIDER_COMMAND+x}}\" ] || exit 1\nprintf x >> {}\n/bin/cp {} \"$GROK_AUTH_PATH\"\n", quote(&self.source.home_directory), quote(&self.source.auth_file_path), quote(&self.directory.join("refresh-count")), quote(&replacement));
        std::fs::write(&executable, script).unwrap();
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
        self.source.executable_path = Some(executable);
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.directory);
    }
}

#[tokio::test]
async fn valid_source_works_without_cli_or_copied_tokens() {
    let fixture = Fixture::new();
    fixture.write("live-token", "account-a", now_millis() + 3_600_000);
    let credentials = fixture.credentials();
    let transient = resolve_credentials(&credentials, false).await.unwrap();
    assert_eq!(transient.access_token, "live-token");
    assert_eq!(
        transient.extras[VERSION_KEY].as_str(),
        Some(xai_grok_client_version())
    );
    assert!(credentials.access_token.is_empty());
    assert!(credentials.refresh_token.is_none());
}

#[tokio::test]
async fn source_identity_expiry_and_invalid_metadata_fail_closed() {
    let fixture = Fixture::new();
    let mut credentials = fixture.credentials();
    credentials.access_token = "stale-nora-token".into();
    credentials.refresh_token = Some("stale-nora-refresh".into());
    fixture.write("other-token", "account-b", now_millis() + 3_600_000);
    assert_eq!(
        resolve_credentials(&credentials, false).await.err(),
        Some(SourceError::AccountChanged)
    );
    fixture.write("expired-token", "account-a", now_millis() - 1);
    assert_eq!(
        resolve_credentials(&credentials, false).await.err(),
        Some(SourceError::CliUnavailable)
    );
    std::fs::remove_file(&fixture.source.auth_file_path).unwrap();
    assert!(resolve_credentials(&credentials, true).await.is_err());
    credentials
        .extras
        .insert(METADATA_KEY.into(), JsonValue::string("invalid"));
    assert!(resolve_credentials(&credentials, false).await.is_err());
}

#[cfg(unix)]
#[tokio::test]
async fn expired_sources_refresh_once_and_keep_the_custom_auth_path_and_detected_version() {
    let mut fixture = Fixture::new();
    fixture.cli();
    fixture.write("expired-token", "account-a", now_millis() - 1);
    let credentials = Arc::new(fixture.credentials());
    let mut requests = tokio::task::JoinSet::new();
    for _ in 0..10 {
        let credentials = credentials.clone();
        requests.spawn(async move { resolve_credentials(&credentials, false).await });
    }
    while let Some(result) = requests.join_next().await {
        let transient = result.unwrap().unwrap();
        assert_eq!(transient.access_token, "fresh-token");
        assert_eq!(transient.extras[VERSION_KEY].as_str(), Some("99.0.0"));
        assert!(transient.refresh_token.is_none());
    }
    assert_eq!(
        std::fs::read_to_string(fixture.directory.join("refresh-count")).unwrap(),
        "x"
    );
    assert!(credentials.access_token.is_empty());
}

#[test]
fn source_environment_removes_auth_overrides_and_versions_never_downgrade() {
    let fixture = Fixture::new();
    let environment = command_environment(
        &fixture.source,
        [
            ("GROK_AUTH_PATH".into(), "/wrong".into()),
            ("GROK_AUTH_PROVIDER_COMMAND".into(), "untrusted".into()),
            ("HTTPS_PROXY".into(), "http://proxy.test".into()),
            ("https_proxy".into(), "http://lower.test".into()),
            ("NO_PROXY".into(), "*".into()),
        ],
        Some(&NetworkProxy::Direct),
    );
    assert_eq!(
        environment.get(std::ffi::OsStr::new("GROK_AUTH_PATH")),
        Some(&fixture.source.auth_file_path.as_os_str().to_owned())
    );
    assert!(!environment.contains_key(std::ffi::OsStr::new("GROK_AUTH_PROVIDER_COMMAND")));
    assert!(!environment.contains_key(std::ffi::OsStr::new("HTTPS_PROXY")));
    assert!(!environment.contains_key(std::ffi::OsStr::new("https_proxy")));
    let mut credentials = fixture.credentials();
    for candidate in ["0.2.117", "invalid", "1.2.3.4"] {
        credentials
            .extras
            .insert(VERSION_KEY.into(), JsonValue::string(candidate));
        assert_eq!(client_version(&credentials), xai_grok_client_version());
    }
}

#[cfg(unix)]
#[tokio::test]
async fn binding_preserves_the_detected_cli_version_without_saving_its_session() {
    use async_trait::async_trait;
    use nora_ai::{HttpClientError, HttpRequest, HttpResponse, HttpStreamingResponse};

    struct UserClient;

    #[async_trait]
    impl HttpClient for UserClient {
        async fn stream(
            &self,
            _request: HttpRequest,
        ) -> Result<HttpStreamingResponse, HttpClientError> {
            unreachable!("account verification uses a single response")
        }

        async fn request(&self, request: HttpRequest) -> Result<HttpResponse, HttpClientError> {
            assert_eq!(request.headers["authorization"], "Bearer local-session");
            Ok(HttpResponse {
                status: 200,
                headers: BTreeMap::new(),
                body: serde_json::to_vec(
                    &json!({"userId":"account-a","email":"person@example.com"}),
                )
                .unwrap(),
            })
        }
    }

    let mut fixture = Fixture::new();
    capture_environment(&mut fixture);
    fixture.write("local-session", "account-a", now_millis() + 3_600_000);
    let session = fixture.source.read("account-a").await.unwrap();
    assert_eq!(session.token, "local-session");
    assert!(session.expires_at.unwrap() > now_millis() + 300_000);
    let proxy = NetworkProxy::Proxy {
        url: "https://private-user:private-password@proxy.invalid:8443".into(),
    };
    let bound = bind_source(fixture.source.clone(), &UserClient, Some(&proxy))
        .await
        .unwrap();
    assert!(!fixture.directory.join("refresh-count").exists());
    assert_eq!(
        fixture.source.read("account-a").await.unwrap().token,
        "local-session"
    );
    assert_eq!(bound.extras[VERSION_KEY].as_str(), Some("99.0.0"));
    assert_eq!(bound.extras["email"].as_str(), Some("person@example.com"));
    assert!(bound.access_token.is_empty());
    assert!(bound.refresh_token.is_none());
    assert!(bound.expires_at.is_none());
    let saved = serde_json::to_string(&bound).unwrap();
    assert!(!saved.contains("local-session"));
    assert!(!saved.contains("private-password"));
    let captured = std::fs::read_to_string(fixture.directory.join("command-env")).unwrap();
    assert!(captured
        .lines()
        .take(6)
        .all(|value| value == "https://private-user:private-password@proxy.invalid:8443"));
}

#[test]
fn explicit_proxy_replaces_inherited_proxies_and_bypass_lists() {
    let fixture = Fixture::new();
    let proxy = NetworkProxy::Proxy {
        url: "socks5h://private-user:private-password@proxy.invalid:1080".into(),
    };
    let environment = command_environment(
        &fixture.source,
        [
            ("HTTP_PROXY".into(), "http://old.invalid".into()),
            ("https_proxy".into(), "http://other.invalid".into()),
            ("ALL_PROXY".into(), "http://third.invalid".into()),
            ("No_PrOxY".into(), "*".into()),
        ],
        Some(&proxy),
    );
    for key in [
        "HTTP_PROXY",
        "http_proxy",
        "HTTPS_PROXY",
        "https_proxy",
        "ALL_PROXY",
        "all_proxy",
    ] {
        assert!(environment.get(std::ffi::OsStr::new(key)).is_some_and(
            |value| value == "socks5h://private-user:private-password@proxy.invalid:1080"
        ));
    }
    for key in ["NO_PROXY", "no_proxy"] {
        assert!(environment
            .get(std::ffi::OsStr::new(key))
            .is_some_and(|value| value.is_empty()));
    }
    assert!(!environment.contains_key(std::ffi::OsStr::new("No_PrOxY")));
}

#[cfg(unix)]
fn capture_environment(fixture: &mut Fixture) {
    fixture.cli();
    let executable = fixture.source.executable_path.as_ref().unwrap();
    let path = fixture.directory.join("command-env");
    let quoted = format!("'{}'", path.to_string_lossy().replace('\'', "'\\''"));
    let script = std::fs::read_to_string(executable).unwrap();
    let capture = format!("#!/bin/sh\nprintf '%s\\n' \"${{HTTP_PROXY-<unset>}}\" \"${{http_proxy-<unset>}}\" \"${{HTTPS_PROXY-<unset>}}\" \"${{https_proxy-<unset>}}\" \"${{ALL_PROXY-<unset>}}\" \"${{all_proxy-<unset>}}\" \"${{NO_PROXY-<unset>}}\" \"${{no_proxy-<unset>}}\" > {quoted}\n");
    std::fs::write(executable, script.replacen("#!/bin/sh\n", &capture, 1)).unwrap();
}

#[cfg(unix)]
#[tokio::test]
async fn actual_cli_refresh_and_version_receive_the_selected_network_policy() {
    for proxy in [
        None,
        Some(NetworkProxy::Direct),
        Some(NetworkProxy::Proxy {
            url: "http://private-user:private-password@proxy.invalid:8080".into(),
        }),
        Some(NetworkProxy::Proxy {
            url: "https://private-user:private-password@proxy.invalid:443".into(),
        }),
        Some(NetworkProxy::Proxy {
            url: "socks5://private-user:private-password@proxy.invalid:1080".into(),
        }),
        Some(NetworkProxy::Proxy {
            url: "socks5h://private-user:private-password@proxy.invalid:1080".into(),
        }),
    ] {
        let mut fixture = Fixture::new();
        capture_environment(&mut fixture);
        fixture.write("expired-token", "account-a", now_millis() - 1);
        let credentials = fixture.credentials();
        let transient = resolve_credentials_with_network(&credentials, false, proxy.as_ref())
            .await
            .unwrap();
        assert!(transient.access_token == "fresh-token");
        assert!(!serde_json::to_string(&transient)
            .unwrap()
            .contains("private-password"));
        for arguments in [&["models"][..], &["--version"][..]] {
            fixture
                .source
                .command(arguments, Duration::from_secs(3), false, proxy.as_ref())
                .await
                .unwrap();
            let captured = std::fs::read_to_string(fixture.directory.join("command-env")).unwrap();
            let lines = captured.lines().collect::<Vec<_>>();
            assert!(lines.len() == 8);
            match &proxy {
                Some(NetworkProxy::Proxy { url }) => {
                    assert!(lines[..6].iter().all(|line| *line == url));
                    assert!(lines[6..].iter().all(|line| line.is_empty()));
                }
                Some(NetworkProxy::Direct) | None => {
                    assert!(lines[..6].iter().all(|line| *line == "<unset>"));
                    assert!(lines[6..].iter().all(|line| *line == "*"));
                }
            }
        }
    }
}

#[cfg(unix)]
#[tokio::test]
async fn invalid_proxy_never_starts_the_cli_and_failures_do_not_expose_output() {
    use std::os::unix::fs::PermissionsExt;
    let mut fixture = Fixture::new();
    capture_environment(&mut fixture);
    fixture.write("expired-token", "account-a", now_millis() - 1);
    let bad = NetworkProxy::Proxy {
        url: "ftp://private-user:private-password@proxy.invalid".into(),
    };
    let error = resolve_credentials_with_network(&fixture.credentials(), true, Some(&bad))
        .await
        .err()
        .unwrap();
    assert_eq!(error, SourceError::InvalidNetworkProxy);
    assert!(!fixture.directory.join("command-env").exists());
    let executable = fixture.source.executable_path.as_ref().unwrap();
    std::fs::write(
        executable,
        "#!/bin/sh\nprintf '%s' \"$HTTPS_PROXY\"\nprintf '%s' \"$HTTPS_PROXY\" >&2\nexit 1\n",
    )
    .unwrap();
    std::fs::set_permissions(executable, std::fs::Permissions::from_mode(0o700)).unwrap();
    let proxy = NetworkProxy::Proxy {
        url: "http://private-user:private-password@proxy.invalid".into(),
    };
    let error = resolve_credentials_with_network(&fixture.credentials(), true, Some(&proxy))
        .await
        .err()
        .unwrap();
    assert_eq!(error, SourceError::RefreshFailed);
    let visible = format!("{error:?}");
    assert!(!visible.contains("private-user"));
    assert!(!visible.contains("private-password"));
}

struct DiscoveryClient {
    status: u16,
    subject: &'static str,
}

#[async_trait::async_trait]
impl HttpClient for DiscoveryClient {
    async fn stream(
        &self,
        _request: nora_ai::HttpRequest,
    ) -> Result<nora_ai::HttpStreamingResponse, nora_ai::HttpClientError> {
        unreachable!("local account discovery uses /user only")
    }

    async fn request(
        &self,
        request: nora_ai::HttpRequest,
    ) -> Result<nora_ai::HttpResponse, nora_ai::HttpClientError> {
        assert_eq!(request.url, crate::XAI_OAUTH_USER_URL);
        assert_eq!(
            request.headers["authorization"],
            "Bearer local-discovery-token"
        );
        Ok(nora_ai::HttpResponse {
            status: self.status,
            headers: BTreeMap::new(),
            body: serde_json::to_vec(&json!({"userId":self.subject,"email":"person@example.test"}))
                .unwrap(),
        })
    }
}

#[tokio::test]
async fn discovery_verifies_the_current_account_without_copying_tokens_or_running_cli_commands() {
    let mut fixture = Fixture::new();
    #[cfg(unix)]
    fixture.cli();
    fixture.write("local-discovery-token", "account-a", now_millis() + 30_000);
    let original = std::fs::read(&fixture.source.auth_file_path).unwrap();
    let account = discover_source(
        fixture.source.clone(),
        &DiscoveryClient {
            status: 200,
            subject: "account-a",
        },
        None,
    )
    .await
    .unwrap()
    .unwrap();
    assert!(is_bound(&account));
    assert_eq!(account.extras["userId"].as_str(), Some("account-a"));
    assert!(account.access_token.is_empty());
    assert!(account.refresh_token.is_none());
    assert!(!fixture.directory.join("refresh-count").exists());
    assert_eq!(
        std::fs::read(&fixture.source.auth_file_path).unwrap(),
        original
    );
}

#[tokio::test]
async fn discovery_only_reports_signed_out_when_the_local_source_is_definitely_absent() {
    let fixture = Fixture::new();
    let client = DiscoveryClient {
        status: 200,
        subject: "account-a",
    };
    assert!(discover_source(fixture.source.clone(), &client, None)
        .await
        .unwrap()
        .is_none());
    std::fs::write(&fixture.source.auth_file_path, "broken-json").unwrap();
    assert!(discover_source(fixture.source.clone(), &client, None)
        .await
        .is_err());
    fixture.write(
        "local-discovery-token",
        "account-a",
        now_millis() + 3_600_000,
    );
    assert!(discover_source(
        fixture.source.clone(),
        &DiscoveryClient {
            status: 503,
            subject: "account-a"
        },
        None
    )
    .await
    .is_err());
    assert!(discover_source(
        fixture.source.clone(),
        &DiscoveryClient {
            status: 200,
            subject: "account-b"
        },
        None
    )
    .await
    .is_err());
    fixture.write("expired-token", "account-a", now_millis() - 1);
    assert!(discover_source(fixture.source.clone(), &client, None)
        .await
        .is_err());
}
