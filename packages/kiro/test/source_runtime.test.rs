use super::*;
use base64::Engine;
use std::sync::atomic::{AtomicUsize, Ordering};
#[path = "fixture.rs"]
mod fixture;

struct Directory(std::path::PathBuf);
impl Directory {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "nora-kiro-renewal-{}",
            nora_ai::oauth_pkce::random_hex(12)
        ));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }
    fn path(&self, kind: &str) -> String {
        self.0
            .join(if kind == "cli" {
                "data.sqlite3"
            } else {
                "kiro-auth-token.json"
            })
            .to_string_lossy()
            .into()
    }
    fn write(&self, kind: &str, access: &str, expires: i64) {
        write(&self.path(kind), kind, access, "private-refresh", expires);
    }
    fn saved(&self, kind: &str) -> Value {
        let mut credential = session::read_source(
            &SourceKind::from_reference(&json!({"kind":kind})).unwrap(),
            &self.path(kind),
        )
        .unwrap();
        credential.email = "person@example.test".into();
        credential.identity = credential.legacy_identity();
        Kiro::source_saved(
            &credential,
            &SourceKind::from_reference(&json!({"kind":kind})).unwrap(),
            &self.path(kind),
        )
    }
}
impl Drop for Directory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
fn write(path: &str, kind: &str, access: &str, refresh: &str, expires: i64) {
    if kind == "cli" {
        let db = rusqlite::Connection::open(path).unwrap();
        db.execute_batch(
            "CREATE TABLE IF NOT EXISTS auth_kv(key TEXT PRIMARY KEY,value TEXT NOT NULL)",
        )
        .unwrap();
        db.execute("INSERT OR REPLACE INTO auth_kv VALUES(?1,?2)", ["kirocli:social:token", &json!({"access_token":access,"refresh_token":refresh,"expires_at":expires,"region":"eu-central-1","profile_arn":fixture::PROFILE,"preserved":"owner-field"}).to_string()]).unwrap();
    } else {
        std::fs::write(path,json!({"accessToken":access,"refreshToken":refresh,"expiresAt":expires,"authMethod":"social","region":"eu-central-1","profileArn":fixture::PROFILE,"preserved":"owner-field"}).to_string()).unwrap();
    }
}
struct Owner {
    owns: bool,
    path: Option<String>,
    calls: AtomicUsize,
    fail: bool,
}
#[async_trait]
impl owner::CliOwner for Owner {
    fn owns(&self, _path: &Path) -> bool {
        self.owns
    }
    async fn renew(&self, _proxy: Option<&NetworkProxy>) -> PluginResult<()> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if let Some(path) = &self.path {
            write(
                path,
                "cli",
                "owner-access",
                "owner-refresh",
                session::now() + 3_600_000,
            );
        }
        if self.fail {
            return Err(session::expired());
        }
        Ok(())
    }
}
fn provider(client: Arc<dyn HttpClient>, owner: Arc<Owner>) -> Kiro {
    let mut provider = Kiro::default();
    provider.client = Some(client);
    provider.owner = owner;
    *provider.config.write().unwrap() = Some(Config {
        proxy: None,
        methods: json!([]),
        installation: "fixture".into(),
    });
    provider
}
fn no_owner() -> Arc<Owner> {
    Arc::new(Owner {
        owns: false,
        path: None,
        calls: AtomicUsize::new(0),
        fail: true,
    })
}
fn refreshed() -> fixture::Reply {
    fixture::Reply::json(
        json!({"accessToken":"next-access","refreshToken":"next-refresh","expiresIn":3600}),
    )
}
async fn live(
    provider: &Kiro,
    directory: &Directory,
    kind: &str,
    client: &dyn HttpClient,
    saved: Option<&Value>,
) -> PluginResult<Option<Credential>> {
    provider
        .source_live(
            &SourceKind::from_reference(&json!({"kind":kind})).unwrap(),
            &directory.path(kind),
            client,
            saved,
            false,
            None,
            None,
        )
        .await
}
fn fetch(credentials: Value) -> FetchRequest {
    FetchRequest {account:"source".into(),credentials:Some(credentials),network_proxy:None,url:format!("{}/v1/messages",session::BASE),method:"POST".into(),headers:BTreeMap::new(),metadata:BTreeMap::new(),body:Some(base64::engine::general_purpose::STANDARD.encode(serde_json::to_vec(&json!({"model":"auto","stream":true,"messages":[{"role":"user","content":"hello"}]})).unwrap())),body_file:None}
}

#[test]
fn a_selected_custom_source_does_not_belong_to_the_installed_default_cli() {
    let directory = Directory::new();
    directory.write("cli", "valid-access", session::now() + 3_600_000);
    assert!(!owner::CliOwner::owns(
        &owner::InstalledCli,
        Path::new(&directory.path("cli"))
    ));
}

#[tokio::test]
async fn first_discovery_recovers_an_expired_source_and_keeps_tokens_only_in_the_original_file() {
    let directory = Directory::new();
    directory.write("ide", "expired-access", session::now() - 1);
    let client = Arc::new(fixture::Client::new(vec![
        refreshed(),
        fixture::Reply::json(fixture::usage()),
    ]));
    let provider = provider(client.clone(), no_owner());
    let sources = vec![discovery::Source {
        kind: SourceKind::Ide,
        path: directory.path("ide").into(),
    }];
    let result = discovery::discover_sources(&provider, &sources, client.as_ref(), None)
        .await
        .unwrap();
    assert_eq!(result["accounts"].as_array().unwrap().len(), 1);
    assert!(result.get("errors").is_none());
    assert!(!result.to_string().contains("next-access"));
    assert!(!result.to_string().contains("next-refresh"));
    let stored = session::read_source(&SourceKind::Ide, &directory.path("ide")).unwrap();
    assert_eq!(stored.access, "next-access");
    assert_eq!(stored.refresh, "next-refresh");
    let file: Value =
        serde_json::from_slice(&std::fs::read(directory.path("ide")).unwrap()).unwrap();
    assert_eq!(file["preserved"], "owner-field");
}

#[tokio::test]
async fn a_valid_source_without_expiry_or_refresh_is_verified_without_renewal_or_writes() {
    let directory = Directory::new();
    directory.write("ide", "unknown-expiry", 0);
    let path = directory.path("ide");
    let mut value: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    value.as_object_mut().unwrap().remove("refreshToken");
    std::fs::write(&path, value.to_string()).unwrap();
    let before = std::fs::read(&path).unwrap();
    let client = Arc::new(fixture::Client::new(vec![fixture::Reply::json(
        fixture::usage(),
    )]));
    let provider = provider(client.clone(), no_owner());
    assert_eq!(
        live(&provider, &directory, "ide", client.as_ref(), None)
            .await
            .unwrap()
            .unwrap()
            .access,
        "unknown-expiry"
    );
    assert_eq!(std::fs::read(path).unwrap(), before);
    assert_eq!(client.requests.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn a_rejected_unexpired_or_undated_source_is_renewed_once_before_verification() {
    for expires in [0, session::now() + 3_600_000] {
        for status in [401, 403] {
            let directory = Directory::new();
            directory.write("ide", "rejected-access", expires);
            let client = Arc::new(fixture::Client::new(vec![
                fixture::Reply::raw(status, b"rejected".to_vec()),
                refreshed(),
                fixture::Reply::json(fixture::usage()),
            ]));
            let plugin = provider(client.clone(), no_owner());
            let credential = live(&plugin, &directory, "ide", client.as_ref(), None)
                .await
                .unwrap()
                .unwrap();
            assert_eq!(credential.access, "next-access");
            assert_eq!(credential.email, "person@example.test");
            let requests = client.requests.lock().unwrap();
            assert_eq!(requests.len(), 3);
            assert_eq!(
                requests[0].headers["authorization"],
                "Bearer rejected-access"
            );
            assert!(requests[1].url.ends_with("/refreshToken"));
            assert_eq!(requests[2].headers["authorization"], "Bearer next-access");
            let stored = session::read_source(&SourceKind::Ide, &directory.path("ide")).unwrap();
            assert_eq!(stored.access, "next-access");
            assert_eq!(stored.refresh, "next-refresh");
        }
    }
}

#[tokio::test]
async fn a_rejected_unexpired_cli_source_asks_its_owner_before_direct_renewal() {
    let directory = Directory::new();
    directory.write("cli", "rejected-access", session::now() + 3_600_000);
    let client = Arc::new(fixture::Client::new(vec![
        fixture::Reply::raw(401, b"rejected".to_vec()),
        fixture::Reply::json(fixture::usage()),
    ]));
    let owner = Arc::new(Owner {
        owns: true,
        path: Some(directory.path("cli")),
        calls: AtomicUsize::new(0),
        fail: false,
    });
    let plugin = provider(client.clone(), owner.clone());
    let credential = live(&plugin, &directory, "cli", client.as_ref(), None)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(credential.access, "owner-access");
    assert_eq!(owner.calls.load(Ordering::SeqCst), 1);
    let requests = client.requests.lock().unwrap();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[1].headers["authorization"], "Bearer owner-access");
}

#[tokio::test]
async fn a_repeated_authentication_rejection_stops_after_one_renewal() {
    for status in [401, 403] {
        let directory = Directory::new();
        directory.write("ide", "rejected-access", session::now() + 3_600_000);
        let client = Arc::new(fixture::Client::new(vec![
            fixture::Reply::raw(status, b"rejected".to_vec()),
            refreshed(),
            fixture::Reply::raw(status, b"still rejected".to_vec()),
        ]));
        let plugin = provider(client.clone(), no_owner());
        assert!(live(&plugin, &directory, "ide", client.as_ref(), None)
            .await
            .is_err());
        assert_eq!(client.requests.lock().unwrap().len(), 3);
        let stored = session::read_source(&SourceKind::Ide, &directory.path("ide")).unwrap();
        assert_eq!(stored.access, "next-access");
        assert_eq!(stored.refresh, "next-refresh");
    }
}

#[tokio::test]
async fn a_temporary_identity_outage_does_not_renew_a_current_cli_source() {
    let directory = Directory::new();
    directory.write("cli", "current-access", session::now() + 3_600_000);
    let client = Arc::new(fixture::Client::new(vec![fixture::Reply::raw(
        503,
        b"temporary".to_vec(),
    )]));
    let owner = Arc::new(Owner {
        owns: true,
        path: Some(directory.path("cli")),
        calls: AtomicUsize::new(0),
        fail: false,
    });
    let plugin = provider(client.clone(), owner.clone());
    assert!(live(&plugin, &directory, "cli", client.as_ref(), None)
        .await
        .is_err());
    assert_eq!(owner.calls.load(Ordering::SeqCst), 0);
    assert_eq!(client.requests.lock().unwrap().len(), 1);
    assert_eq!(
        session::read_source(&SourceKind::Cli, &directory.path("cli"))
            .unwrap()
            .access,
        "current-access"
    );
}

#[tokio::test]
async fn a_persisted_rotation_can_renew_again_after_identity_failure_without_replaying_the_old_token(
) {
    for recovery in ["rejected", "forced", "expired"] {
        let directory = Directory::new();
        directory.write("ide", "expired-access", session::now() - 1);
        let mut replies = vec![
            fixture::Reply::json(
                json!({"accessToken":"first-access","refreshToken":"first-refresh","expiresIn":if recovery=="expired"{1}else{3600}}),
            ),
            fixture::Reply::raw(503, b"temporary".to_vec()),
        ];
        if recovery == "rejected" {
            replies.push(fixture::Reply::raw(401, b"rejected".to_vec()));
        }
        replies.extend([refreshed(), fixture::Reply::json(fixture::usage())]);
        let client = Arc::new(fixture::Client::new(replies));
        let plugin = provider(client.clone(), no_owner());
        assert!(live(&plugin, &directory, "ide", client.as_ref(), None)
            .await
            .is_err());
        let stored = session::read_source(&SourceKind::Ide, &directory.path("ide")).unwrap();
        assert_eq!(stored.access, "first-access");
        assert_eq!(stored.refresh, "first-refresh");
        let credential = plugin
            .source_live(
                &SourceKind::Ide,
                &directory.path("ide"),
                client.as_ref(),
                None,
                recovery == "forced",
                Some("first-access"),
                None,
            )
            .await
            .unwrap()
            .unwrap();
        assert_eq!(credential.access, "next-access", "recovery: {recovery}");
        let requests = client.requests.lock().unwrap();
        assert_eq!(requests.len(), if recovery == "rejected" { 5 } else { 4 });
        let refresh_tokens = requests
            .iter()
            .filter(|request| request.url.ends_with("/refreshToken"))
            .map(|request| {
                serde_json::from_slice::<Value>(request.body.as_ref().unwrap()).unwrap()
                    ["refreshToken"]
                    .as_str()
                    .unwrap()
                    .to_owned()
            })
            .collect::<Vec<_>>();
        assert_eq!(refresh_tokens, ["private-refresh", "first-refresh"]);
    }
}

#[tokio::test]
async fn resuming_a_written_rotation_reconciles_the_session_before_verifying_or_renewing_again() {
    for kind in ["cli", "ide"] {
        for recovery in ["current", "forced", "expired"] {
            let directory = Directory::new();
            directory.write(kind, "old-access", session::now() - 1);
            let kind = SourceKind::from_reference(&json!({"kind":kind})).unwrap();
            let path = directory.path(if kind == SourceKind::Cli {
                "cli"
            } else {
                "ide"
            });
            let source = SourceSession::open(&kind, &path, &crate::source::lock_root())
                .unwrap()
                .unwrap();
            let mut next = source.credential().clone();
            next.access = "written-access".into();
            next.refresh = "written-refresh".into();
            next.expires = if recovery == "expired" {
                session::now() - 1
            } else {
                session::now() + 3_600_000
            };
            // The durable write succeeded but the following snapshot reload failed.
            write(
                &path,
                if kind == SourceKind::Cli {
                    "cli"
                } else {
                    "ide"
                },
                &next.access,
                &next.refresh,
                next.expires,
            );
            assert_eq!(source.credential().access, "old-access");
            assert_eq!(source.state(&next).unwrap(), DiskState::Written);
            let mut replies = Vec::new();
            if recovery == "current" {
                replies.push(fixture::Reply::json(fixture::usage()));
            }
            replies.extend([refreshed(), fixture::Reply::json(fixture::usage())]);
            let client = Arc::new(fixture::Client::new(replies));
            let plugin = provider(client.clone(), no_owner());
            let canonical = std::fs::canonicalize(&path).unwrap();
            let source_key = session::fingerprint(&format!(
                "{}\0{}",
                kind.cache_key(),
                canonical.to_string_lossy()
            ));
            plugin.sources.lock().await.insert(
                source_key,
                Arc::new(Mutex::new(SourceState {
                    pending: Some(Renewal {
                        session: source,
                        next,
                    }),
                    verified: None,
                })),
            );
            let mut credential = plugin
                .source_live(
                    &kind,
                    &path,
                    client.as_ref(),
                    None,
                    recovery == "forced",
                    Some("written-access"),
                    None,
                )
                .await
                .unwrap()
                .unwrap();
            if recovery == "current" {
                assert_eq!(credential.access, "written-access");
                assert_eq!(client.requests.lock().unwrap().len(), 1);
                credential = plugin
                    .source_live(
                        &kind,
                        &path,
                        client.as_ref(),
                        None,
                        true,
                        Some("written-access"),
                        None,
                    )
                    .await
                    .unwrap()
                    .unwrap();
            }
            assert_eq!(credential.access, "next-access");
            let durable = session::read_source(&kind, &path).unwrap();
            assert_eq!(durable.access, "next-access");
            assert_eq!(durable.refresh, "next-refresh");
            let requests = client.requests.lock().unwrap();
            assert_eq!(requests.len(), if recovery == "current" { 3 } else { 2 });
            let renewals = requests
                .iter()
                .filter(|request| request.url.ends_with("/refreshToken"))
                .collect::<Vec<_>>();
            assert_eq!(renewals.len(), 1);
            let body: Value = serde_json::from_slice(renewals[0].body.as_ref().unwrap()).unwrap();
            assert_eq!(body["refreshToken"], "written-refresh");
            assert_eq!(
                requests.last().unwrap().headers["authorization"],
                "Bearer next-access"
            );
        }
    }
}

#[tokio::test]
async fn discovery_resumes_a_written_rotation_without_inheriting_the_previous_callers_account() {
    for selected_account in [false, true] {
        let directory = Directory::new();
        let path = directory.path("cli");
        directory.write("cli", "old-access", session::now() - 1);
        let previous_profile = format!("{}-previous", fixture::PROFILE);
        let db = rusqlite::Connection::open(&path).unwrap();
        let raw: String = db
            .query_row(
                "SELECT value FROM auth_kv WHERE key = 'kirocli:social:token'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        let mut value: Value = serde_json::from_str(&raw).unwrap();
        value["profile_arn"] = json!(previous_profile);
        db.execute(
            "UPDATE auth_kv SET value = ?1 WHERE key = 'kirocli:social:token'",
            [value.to_string()],
        )
        .unwrap();
        drop(db);
        let mut source = SourceSession::open(&SourceKind::Cli, &path, &crate::source::lock_root())
            .unwrap()
            .unwrap();
        let mut previous = source.credential().clone();
        previous.email = "person@example.test".into();
        previous.identity = previous.legacy_identity();
        let saved = previous.saved();
        source.bind_identity(&saved).unwrap();
        let mut next = source.credential().clone();
        next.access = "written-access".into();
        next.refresh = "written-refresh".into();
        next.profile = fixture::PROFILE.into();
        write(&path, "cli", &next.access, &next.refresh, next.expires);
        assert_eq!(source.state(&next).unwrap(), DiskState::Written);
        let client = Arc::new(fixture::Client::new(if selected_account {
            vec![]
        } else {
            vec![fixture::Reply::json(fixture::usage())]
        }));
        let owner = Arc::new(Owner {
            owns: true,
            path: Some(path.clone()),
            calls: AtomicUsize::new(0),
            fail: false,
        });
        let plugin = provider(client.clone(), owner.clone());
        let canonical = std::fs::canonicalize(&path).unwrap();
        plugin.sources.lock().await.insert(
            session::fingerprint(&format!("cli\0{}", canonical.to_string_lossy())),
            Arc::new(Mutex::new(SourceState {
                pending: Some(Renewal {
                    session: source,
                    next,
                }),
                verified: None,
            })),
        );
        let result = plugin
            .source_live(
                &SourceKind::Cli,
                &path,
                client.as_ref(),
                selected_account.then_some(&saved),
                false,
                None,
                None,
            )
            .await;
        if selected_account {
            assert!(result.is_err());
            assert_eq!(owner.calls.load(Ordering::SeqCst), 0);
            assert!(client.requests.lock().unwrap().is_empty());
        } else {
            let credential = result.unwrap().unwrap();
            assert_eq!(credential.access, "owner-access");
            assert!(!credential.matches_saved_identity(&saved));
            assert_eq!(owner.calls.load(Ordering::SeqCst), 1);
            assert_eq!(client.requests.lock().unwrap().len(), 1);
        }
    }
}

#[tokio::test]
async fn default_cli_owner_renewal_is_preferred_even_if_it_returns_an_error_after_saving() {
    let directory = Directory::new();
    directory.write("cli", "expired-access", session::now() - 1);
    let client = Arc::new(fixture::Client::new(vec![fixture::Reply::json(
        fixture::usage(),
    )]));
    let owner = Arc::new(Owner {
        owns: true,
        path: Some(directory.path("cli")),
        calls: AtomicUsize::new(0),
        fail: true,
    });
    let provider = provider(client.clone(), owner.clone());
    assert_eq!(
        live(&provider, &directory, "cli", client.as_ref(), None)
            .await
            .unwrap()
            .unwrap()
            .access,
        "owner-access"
    );
    assert_eq!(owner.calls.load(Ordering::SeqCst), 1);
    assert_eq!(client.requests.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn failed_owner_falls_back_to_source_refresh_and_custom_sources_do_not_run_the_owner() {
    for owns in [true, false] {
        let directory = Directory::new();
        directory.write("cli", "expired-access", session::now() - 1);
        let client = Arc::new(fixture::Client::new(vec![
            refreshed(),
            fixture::Reply::json(fixture::usage()),
        ]));
        let owner = Arc::new(Owner {
            owns,
            path: None,
            calls: AtomicUsize::new(0),
            fail: true,
        });
        let provider = provider(client.clone(), owner.clone());
        assert_eq!(
            live(&provider, &directory, "cli", client.as_ref(), None)
                .await
                .unwrap()
                .unwrap()
                .access,
            "next-access"
        );
        assert_eq!(owner.calls.load(Ordering::SeqCst), usize::from(owns));
        assert!(client.requests.lock().unwrap()[0]
            .url
            .ends_with("/refreshToken"));
    }
}

#[tokio::test]
async fn a_rotated_token_survives_failed_identity_verification_without_replaying_refresh_or_deadlocking(
) {
    let directory = Directory::new();
    directory.write("ide", "expired-access", session::now() - 1);
    let client = Arc::new(fixture::Client::new(vec![
        refreshed(),
        fixture::Reply::raw(503, b"temporary".to_vec()),
        fixture::Reply::json(fixture::usage()),
    ]));
    let provider = provider(client.clone(), no_owner());
    assert!(live(&provider, &directory, "ide", client.as_ref(), None)
        .await
        .is_err());
    assert_eq!(
        session::read_source(&SourceKind::Ide, &directory.path("ide"))
            .unwrap()
            .access,
        "next-access"
    );
    let result = tokio::time::timeout(
        Duration::from_secs(2),
        live(&provider, &directory, "ide", client.as_ref(), None),
    )
    .await
    .unwrap()
    .unwrap()
    .unwrap();
    assert_eq!(result.access, "next-access");
    assert_eq!(client.requests.lock().unwrap().len(), 3);
    assert_eq!(
        session::read_source(&SourceKind::Ide, &directory.path("ide"))
            .unwrap()
            .refresh,
        "next-refresh"
    );
}

#[tokio::test]
async fn restart_after_identity_failure_child() {
    let Ok(path) = std::env::var("NORA_KIRO_RESTART_TEST_SOURCE") else {
        return;
    };
    let kind = SourceKind::from_reference(
        &json!({"kind":std::env::var("NORA_KIRO_RESTART_TEST_KIND").unwrap()}),
    )
    .unwrap();
    let disk = session::read_source(&kind, &path).unwrap();
    assert_eq!(disk.access, "next-access");
    assert_eq!(disk.refresh, "next-refresh");
    let client = Arc::new(fixture::Client::new(vec![fixture::Reply::json(
        fixture::usage(),
    )]));
    let plugin = provider(client.clone(), no_owner());
    let credential = plugin
        .source_live(&kind, &path, client.as_ref(), None, false, None, None)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(credential.access, "next-access");
    assert_eq!(credential.refresh, "next-refresh");
    let requests = client.requests.lock().unwrap();
    assert_eq!(requests.len(), 1);
    assert!(requests[0].url.contains("Get-Usage-Limits"));
    assert_eq!(requests[0].headers["authorization"], "Bearer next-access");
    std::fs::write(format!("{path}.restart-verified"), b"verified").unwrap();
}

#[tokio::test]
async fn a_new_plugin_process_uses_the_saved_rotated_pair_after_identity_service_failure() {
    for kind in ["cli", "ide"] {
        let directory = Directory::new();
        directory.write(kind, "expired-access", session::now() - 1);
        let client = Arc::new(fixture::Client::new(vec![
            refreshed(),
            fixture::Reply::raw(503, b"temporary identity outage".to_vec()),
        ]));
        let plugin = provider(client.clone(), no_owner());
        assert!(live(&plugin, &directory, kind, client.as_ref(), None)
            .await
            .is_err());
        let disk = session::read_source(
            &SourceKind::from_reference(&json!({"kind":kind})).unwrap(),
            &directory.path(kind),
        )
        .unwrap();
        assert_eq!(disk.access, "next-access");
        assert_eq!(disk.refresh, "next-refresh");
        assert_eq!(client.requests.lock().unwrap().len(), 2);
        drop(plugin);
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "provider::source_runtime::tests::restart_after_identity_failure_child",
                "--nocapture",
            ])
            .env("NORA_KIRO_RESTART_TEST_SOURCE", directory.path(kind))
            .env("NORA_KIRO_RESTART_TEST_KIND", kind)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "restart child failed: {}",
            String::from_utf8_lossy(&output.stdout)
        );
        assert_eq!(
            std::fs::read(format!("{}.restart-verified", directory.path(kind))).unwrap(),
            b"verified"
        );
    }
}

#[tokio::test]
async fn a_saved_magpie_rotation_with_failed_identity_verification_does_not_lock_other_accounts() {
    let directory = Directory::new();
    let path = directory.0.join("plugin-auth.json");
    std::fs::write(&path, json!({
        "kiro#a":{"type":"oauth","access":"expired-a-access","refresh":"private-a-refresh","expires":session::now()-1,"method":"social","region":"eu-central-1","profileArn":fixture::PROFILE},
        "kiro#b":{"type":"oauth","access":"current-b-access","refresh":"private-b-refresh","expires":session::now()+3_600_000,"method":"social","region":"eu-central-1","profileArn":fixture::PROFILE}
    }).to_string()).unwrap();
    let client = Arc::new(fixture::Client::new(vec![
        refreshed(),
        fixture::Reply::raw(503, b"temporary identity failure".to_vec()),
        fixture::Reply::json(fixture::usage()),
        fixture::Reply::json(fixture::usage()),
    ]));
    let plugin = provider(client.clone(), no_owner());
    let path = path.to_str().unwrap();
    assert!(plugin
        .source_live(
            &SourceKind::Magpie(crate::source_kind::MagpieAccount::new("kiro#a").unwrap()),
            path,
            client.as_ref(),
            None,
            false,
            None,
            None
        )
        .await
        .is_err());
    assert_eq!(
        session::read_source(
            &SourceKind::Magpie(crate::source_kind::MagpieAccount::new("kiro#a").unwrap()),
            path
        )
        .unwrap()
        .refresh,
        "next-refresh"
    );
    let other = tokio::time::timeout(
        Duration::from_secs(2),
        plugin.source_live(
            &SourceKind::Magpie(crate::source_kind::MagpieAccount::new("kiro#b").unwrap()),
            path,
            client.as_ref(),
            None,
            false,
            None,
            None,
        ),
    )
    .await
    .unwrap()
    .unwrap()
    .unwrap();
    assert_eq!(other.access, "current-b-access");
    let first = plugin
        .source_live(
            &SourceKind::Magpie(crate::source_kind::MagpieAccount::new("kiro#a").unwrap()),
            path,
            client.as_ref(),
            None,
            false,
            None,
            None,
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(first.access, "next-access");
    let requests = client.requests.lock().unwrap();
    assert_eq!(requests.len(), 4);
    assert_eq!(
        requests
            .iter()
            .filter(|request| request.url.ends_with("/refreshToken"))
            .count(),
        1
    );
    assert_eq!(
        requests[2].headers["authorization"],
        "Bearer current-b-access"
    );
    assert_eq!(requests[3].headers["authorization"], "Bearer next-access");
}

struct SwitchingSourceClient {
    inner: fixture::Client,
    path: String,
    kind: String,
}

struct PausingIdentityClient {
    inner: fixture::Client,
    entered: tokio::sync::Notify,
    release: tokio::sync::Notify,
}

#[async_trait]
impl HttpClient for PausingIdentityClient {
    async fn stream(
        &self,
        request: nora_ai::HttpRequest,
    ) -> Result<nora_ai::HttpStreamingResponse, nora_ai::HttpClientError> {
        if request.headers.get("authorization").map(String::as_str)
            == Some("Bearer current-a-access")
        {
            self.entered.notify_one();
            self.release.notified().await;
        }
        self.inner.stream(request).await
    }
}

#[tokio::test]
async fn an_unexpired_magpie_account_does_not_lock_other_accounts_while_identifying() {
    let directory = Directory::new();
    let path = directory.0.join("plugin-auth.json");
    std::fs::write(&path, json!({
        "kiro#a":{"type":"oauth","access":"current-a-access","refresh":"private-a-refresh","expires":session::now()+3_600_000,"method":"social","region":"eu-central-1","profileArn":fixture::PROFILE},
        "kiro#b":{"type":"oauth","access":"current-b-access","refresh":"private-b-refresh","expires":session::now()+3_600_000,"method":"social","region":"eu-central-1","profileArn":fixture::PROFILE}
    }).to_string()).unwrap();
    let client = Arc::new(PausingIdentityClient {
        inner: fixture::Client::new(vec![
            fixture::Reply::json(fixture::usage()),
            fixture::Reply::json(fixture::usage()),
        ]),
        entered: tokio::sync::Notify::new(),
        release: tokio::sync::Notify::new(),
    });
    let plugin = provider(client.clone(), no_owner());
    let first_kind = SourceKind::Magpie(crate::source_kind::MagpieAccount::new("kiro#a").unwrap());
    let second_kind = SourceKind::Magpie(crate::source_kind::MagpieAccount::new("kiro#b").unwrap());
    let first = plugin.source_live(
        &first_kind,
        path.to_str().unwrap(),
        client.as_ref(),
        None,
        false,
        None,
        None,
    );
    tokio::pin!(first);
    tokio::select! {
        _ = &mut first => panic!("The first identity request should wait for release"),
        _ = client.entered.notified() => {},
    }
    let second = tokio::time::timeout(
        Duration::from_secs(2),
        plugin.source_live(
            &second_kind,
            path.to_str().unwrap(),
            client.as_ref(),
            None,
            false,
            None,
            None,
        ),
    )
    .await
    .unwrap()
    .unwrap()
    .unwrap();
    assert_eq!(second.access, "current-b-access");
    client.release.notify_one();
    assert_eq!(first.await.unwrap().unwrap().access, "current-a-access");
    assert_eq!(client.inner.requests.lock().unwrap().len(), 2);
}
#[async_trait]
impl HttpClient for SwitchingSourceClient {
    async fn stream(
        &self,
        request: nora_ai::HttpRequest,
    ) -> Result<nora_ai::HttpStreamingResponse, nora_ai::HttpClientError> {
        if request.url.contains("Get-Usage-Limits") {
            write(
                &self.path,
                &self.kind,
                "owner-switched-access",
                "owner-switched-refresh",
                session::now() + 3_600_000,
            );
        }
        self.inner.stream(request).await
    }
}

#[tokio::test]
async fn a_source_switch_during_identity_verification_also_refuses_an_unexpired_pair() {
    for kind in ["cli", "ide"] {
        let directory = Directory::new();
        directory.write(kind, "current-access", session::now() + 3_600_000);
        let client = Arc::new(SwitchingSourceClient {
            inner: fixture::Client::new(vec![fixture::Reply::json(fixture::usage())]),
            path: directory.path(kind),
            kind: kind.into(),
        });
        let plugin = provider(client.clone(), no_owner());
        assert!(live(&plugin, &directory, kind, client.as_ref(), None)
            .await
            .is_err());
        let current = session::read_source(
            &SourceKind::from_reference(&json!({"kind":kind})).unwrap(),
            &directory.path(kind),
        )
        .unwrap();
        assert_eq!(current.access, "owner-switched-access");
        assert_eq!(current.refresh, "owner-switched-refresh");
        assert_eq!(client.inner.requests.lock().unwrap().len(), 1);
    }
}

#[tokio::test]
async fn an_authentication_rejection_during_an_owner_switch_does_not_renew_the_new_account() {
    for kind in ["cli", "ide"] {
        let directory = Directory::new();
        directory.write(kind, "current-access", session::now() + 3_600_000);
        let client = Arc::new(SwitchingSourceClient {
            inner: fixture::Client::new(vec![fixture::Reply::raw(401, b"rejected".to_vec())]),
            path: directory.path(kind),
            kind: kind.into(),
        });
        let plugin = provider(client.clone(), no_owner());
        assert!(live(&plugin, &directory, kind, client.as_ref(), None)
            .await
            .is_err());
        let current = session::read_source(
            &SourceKind::from_reference(&json!({"kind":kind})).unwrap(),
            &directory.path(kind),
        )
        .unwrap();
        assert_eq!(current.access, "owner-switched-access");
        assert_eq!(current.refresh, "owner-switched-refresh");
        assert_eq!(client.inner.requests.lock().unwrap().len(), 1);
    }
}

#[tokio::test]
async fn profile_authentication_rejection_is_recovered_before_the_first_source_identity() {
    let directory = Directory::new();
    directory.write("ide", "rejected-access", session::now() + 3_600_000);
    let path = directory.path("ide");
    let mut saved: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    saved.as_object_mut().unwrap().remove("profileArn");
    std::fs::write(&path, saved.to_string()).unwrap();
    let client = Arc::new(fixture::Client::new(vec![
        fixture::Reply::raw(401, b"rejected".to_vec()),
        fixture::Reply::raw(401, b"rejected".to_vec()),
        fixture::Reply::json(
            json!({"accessToken":"next-access","refreshToken":"next-refresh","expiresIn":3600,"profileArn":fixture::PROFILE}),
        ),
        fixture::Reply::json(fixture::usage()),
    ]));
    let plugin = provider(client.clone(), no_owner());
    let credential = live(&plugin, &directory, "ide", client.as_ref(), None)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(credential.access, "next-access");
    assert_eq!(credential.profile, fixture::PROFILE);
    let requests = client.requests.lock().unwrap();
    assert_eq!(requests.len(), 4);
    assert!(requests[2].url.ends_with("/refreshToken"));
}

#[tokio::test]
async fn a_source_switch_during_identity_verification_refuses_the_previous_rotated_pair() {
    for kind in ["cli", "ide"] {
        let directory = Directory::new();
        directory.write(kind, "expired-access", session::now() - 1);
        let client = Arc::new(SwitchingSourceClient {
            inner: fixture::Client::new(vec![refreshed(), fixture::Reply::json(fixture::usage())]),
            path: directory.path(kind),
            kind: kind.into(),
        });
        let plugin = provider(client.clone(), no_owner());
        assert!(live(&plugin, &directory, kind, client.as_ref(), None)
            .await
            .is_err());
        let current = session::read_source(
            &SourceKind::from_reference(&json!({"kind":kind})).unwrap(),
            &directory.path(kind),
        )
        .unwrap();
        assert_eq!(current.access, "owner-switched-access");
        assert_eq!(current.refresh, "owner-switched-refresh");
        assert_eq!(client.inner.requests.lock().unwrap().len(), 2);
    }
}

#[tokio::test]
async fn concurrent_calls_renew_a_source_only_once() {
    let directory = Directory::new();
    directory.write("ide", "expired-access", session::now() - 1);
    let client = Arc::new(fixture::Client::new(vec![
        refreshed(),
        fixture::Reply::json(fixture::usage()),
    ]));
    let provider = provider(client.clone(), no_owner());
    let (first, second) = tokio::join!(
        live(&provider, &directory, "ide", client.as_ref(), None),
        live(&provider, &directory, "ide", client.as_ref(), None)
    );
    assert_eq!(first.unwrap().unwrap().access, "next-access");
    assert_eq!(second.unwrap().unwrap().access, "next-access");
    assert_eq!(client.requests.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn a_403_reuses_a_token_already_renewed_by_the_owner_instead_of_spending_refresh_again() {
    let directory = Directory::new();
    directory.write("ide", "old-access", session::now() + 3_600_000);
    let saved = directory.saved("ide");
    directory.write("ide", "owner-new-access", session::now() + 3_600_000);
    let client = Arc::new(fixture::Client::new(vec![fixture::Reply::json(
        fixture::usage(),
    )]));
    let provider = provider(client.clone(), no_owner());
    let result = provider
        .source_live(
            &SourceKind::Ide,
            &directory.path("ide"),
            client.as_ref(),
            Some(&saved),
            true,
            Some("old-access"),
            None,
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(result.access, "owner-new-access");
    assert_eq!(client.requests.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn a_source_chat_403_renews_once_and_retries_only_after_original_source_persistence() {
    let directory = Directory::new();
    directory.write("ide", "old-access", session::now() + 3_600_000);
    let saved = directory.saved("ide");
    let client = Arc::new(fixture::Client::new(vec![
        fixture::Reply::json(fixture::usage()),
        fixture::Reply::raw(403, b"denied".to_vec()),
        refreshed(),
        fixture::Reply::json(fixture::usage()),
        fixture::Reply::raw(403, b"still denied".to_vec()),
    ]));
    let provider = provider(client.clone(), no_owner());
    assert_eq!(provider.fetch(fetch(saved)).await.unwrap().status, 403);
    assert_eq!(
        session::read_source(&SourceKind::Ide, &directory.path("ide"))
            .unwrap()
            .access,
        "next-access"
    );
    let requests = client.requests.lock().unwrap();
    assert_eq!(requests.len(), 5);
    assert_eq!(requests[4].headers["authorization"], "Bearer next-access");
    assert_eq!(
        requests
            .iter()
            .filter(|request| request.url.ends_with("/refreshToken"))
            .count(),
        1
    );
}

#[tokio::test]
async fn switched_expired_user_is_renewed_in_its_source_but_never_used_as_the_old_nora_account() {
    let directory = Directory::new();
    directory.write("ide", "user-a-access", session::now() + 3_600_000);
    let mut saved = directory.saved("ide");
    let mut a = session::read_source(&SourceKind::Ide, &directory.path("ide")).unwrap();
    a.user_id = "user-a".into();
    a.email = "person@example.test".into();
    a.identity = a.legacy_identity();
    saved["accountIdentity"] = a.account_identity().unwrap();
    saved["extras"]["accountIdentity"] = saved["accountIdentity"].clone();
    directory.write("ide", "expired-user-b", session::now() - 1);
    let mut b = fixture::usage();
    b["userInfo"]["userId"] = json!("user-b");
    let client = Arc::new(fixture::Client::new(vec![
        refreshed(),
        fixture::Reply::raw(503, b"temporary".to_vec()),
        fixture::Reply::json(b.clone()),
        fixture::Reply::json(b),
    ]));
    let provider = provider(client.clone(), no_owner());
    assert!(
        live(&provider, &directory, "ide", client.as_ref(), Some(&saved))
            .await
            .is_err()
    );
    assert!(
        live(&provider, &directory, "ide", client.as_ref(), Some(&saved))
            .await
            .is_err()
    );
    assert_eq!(
        session::read_source(&SourceKind::Ide, &directory.path("ide"))
            .unwrap()
            .access,
        "next-access"
    );
    let current = live(&provider, &directory, "ide", client.as_ref(), None)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(current.user_id, "user-b");
    assert!(!current.matches_saved_identity(&saved));
    assert_eq!(
        client
            .requests
            .lock()
            .unwrap()
            .iter()
            .filter(|request| request.url.ends_with("/refreshToken"))
            .count(),
        1
    );
}

#[tokio::test]
async fn an_owner_switch_drops_stale_pending_tokens_and_discovery_can_see_the_new_sign_in() {
    let directory = Directory::new();
    directory.write("ide", "expired-access", session::now() - 1);
    let client = Arc::new(fixture::Client::new(vec![
        refreshed(),
        fixture::Reply::raw(503, b"temporary".to_vec()),
        fixture::Reply::json(fixture::usage()),
    ]));
    let provider = provider(client.clone(), no_owner());
    assert!(live(&provider, &directory, "ide", client.as_ref(), None)
        .await
        .is_err());
    directory.write("ide", "owner-third-access", session::now() + 3_600_000);
    let current = live(&provider, &directory, "ide", client.as_ref(), None)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(current.access, "owner-third-access");
    assert_eq!(
        session::read_source(&SourceKind::Ide, &directory.path("ide"))
            .unwrap()
            .access,
        "owner-third-access"
    );
    assert_eq!(client.requests.lock().unwrap().len(), 3);
}

#[tokio::test]
async fn signing_out_clears_pending_without_replaying_refresh_or_leaving_a_phantom_account() {
    let directory = Directory::new();
    directory.write("ide", "expired-access", session::now() - 1);
    let client = Arc::new(fixture::Client::new(vec![
        refreshed(),
        fixture::Reply::raw(503, b"temporary".to_vec()),
    ]));
    let provider = provider(client.clone(), no_owner());
    assert!(live(&provider, &directory, "ide", client.as_ref(), None)
        .await
        .is_err());
    std::fs::remove_file(directory.path("ide")).unwrap();
    assert!(live(&provider, &directory, "ide", client.as_ref(), None)
        .await
        .unwrap()
        .is_none());
    assert_eq!(client.requests.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn full_pending_capacity_refuses_new_rotation_without_dropping_existing_pending_entries() {
    let directory = Directory::new();
    directory.write("cli", "expired-access", session::now() - 1);
    let client = Arc::new(LockedClient {
        inner: fixture::Client::new(vec![refreshed()]),
        path: directory.path("cli"),
        database: std::sync::Mutex::new(None),
    });
    let provider = provider(client.clone(), no_owner());
    assert!(live(&provider, &directory, "cli", client.as_ref(), None)
        .await
        .is_err());
    let original = provider
        .sources
        .lock()
        .await
        .values()
        .next()
        .unwrap()
        .clone();
    assert!(original.lock().await.pending.is_some());
    for index in 0..63 {
        provider.sources.lock().await.insert(
            index.to_string(),
            Arc::new(Mutex::new(SourceState::default())),
        );
    }
    let next = Directory::new();
    next.write("ide", "another-expired", session::now() - 1);
    assert!(live(&provider, &next, "ide", client.as_ref(), None)
        .await
        .is_err());
    assert_eq!(client.inner.requests.lock().unwrap().len(), 1);
    assert_eq!(provider.sources.lock().await.len(), 64);
    assert!(original.lock().await.pending.is_some());
}

struct LockedClient {
    inner: fixture::Client,
    path: String,
    database: std::sync::Mutex<Option<rusqlite::Connection>>,
}
#[async_trait]
impl HttpClient for LockedClient {
    async fn stream(
        &self,
        request: nora_ai::HttpRequest,
    ) -> Result<nora_ai::HttpStreamingResponse, nora_ai::HttpClientError> {
        if request.url.ends_with("/refreshToken") && self.database.lock().unwrap().is_none() {
            let db = rusqlite::Connection::open(&self.path).unwrap();
            db.execute_batch("BEGIN IMMEDIATE").unwrap();
            *self.database.lock().unwrap() = Some(db);
        }
        self.inner.stream(request).await
    }
}

#[tokio::test]
async fn source_save_failure_prevents_chat_and_retry_saves_the_rotated_pair_before_sending() {
    let directory = Directory::new();
    directory.write("cli", "expired-access", session::now() - 1);
    let saved = directory.saved("cli");
    let client = Arc::new(LockedClient {
        inner: fixture::Client::new(vec![
            refreshed(),
            fixture::Reply::json(fixture::usage()),
            fixture::Reply::raw(
                200,
                fixture::event("assistantResponseEvent", json!({"content":"answer"})),
            ),
        ]),
        path: directory.path("cli"),
        database: std::sync::Mutex::new(None),
    });
    let provider = provider(client.clone(), no_owner());
    assert!(provider.fetch(fetch(saved.clone())).await.is_err());
    assert_eq!(client.inner.requests.lock().unwrap().len(), 1);
    assert_eq!(
        session::read_source(&SourceKind::Cli, &directory.path("cli"))
            .unwrap()
            .access,
        "expired-access"
    );
    client
        .database
        .lock()
        .unwrap()
        .take()
        .unwrap()
        .execute_batch("COMMIT")
        .unwrap();
    let result = tokio::time::timeout(Duration::from_secs(5), provider.fetch(fetch(saved)))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(result.status, 200);
    assert!(fixture::body(result).await.contains("answer"));
    assert_eq!(
        session::read_source(&SourceKind::Cli, &directory.path("cli"))
            .unwrap()
            .refresh,
        "next-refresh"
    );
    let requests = client.inner.requests.lock().unwrap();
    assert_eq!(requests.len(), 3);
    assert_eq!(requests[2].headers["authorization"], "Bearer next-access");
}

#[tokio::test]
async fn pending_switched_user_renewal_can_finish_after_save_failure_without_serving_the_old_nora_identity(
) {
    let directory = Directory::new();
    directory.write("cli", "expired-user-b", session::now() - 1);
    let mut a = session::read_source(&SourceKind::Cli, &directory.path("cli")).unwrap();
    a.user_id = "user-a".into();
    a.email = "person@example.test".into();
    a.identity = a.legacy_identity();
    let saved = Kiro::source_saved(&a, &SourceKind::Cli, &directory.path("cli"));
    let mut b = fixture::usage();
    b["userInfo"]["userId"] = json!("user-b");
    let client = Arc::new(LockedClient {
        inner: fixture::Client::new(vec![
            refreshed(),
            fixture::Reply::json(b.clone()),
            fixture::Reply::json(b),
        ]),
        path: directory.path("cli"),
        database: std::sync::Mutex::new(None),
    });
    let provider = provider(client.clone(), no_owner());
    assert!(provider.fetch(fetch(saved.clone())).await.is_err());
    client
        .database
        .lock()
        .unwrap()
        .take()
        .unwrap()
        .execute_batch("COMMIT")
        .unwrap();
    assert!(provider.fetch(fetch(saved)).await.is_err());
    assert_eq!(client.inner.requests.lock().unwrap().len(), 2);
    assert_eq!(
        session::read_source(&SourceKind::Cli, &directory.path("cli"))
            .unwrap()
            .access,
        "next-access"
    );
    let current = live(&provider, &directory, "cli", client.as_ref(), None)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(current.user_id, "user-b");
    assert!(client
        .inner
        .requests
        .lock()
        .unwrap()
        .iter()
        .all(|request| !request.url.contains("generateAssistantResponse")));
}
