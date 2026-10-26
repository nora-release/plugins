use super::*;
use crate::test_support::{jwt, Client, Directory, Reply};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

pub(crate) fn saved(lifetime: i64) -> Value {
    json!({"type":"oauth","authMethod":"oauth","provider":"chatgpt-api","access":"access-one","refresh":"refresh-one","expires":now()+lifetime,"extras":{"clientId":"oaiapp_registered","hostId":"urn:uuid:ce20296b-fb20-4415-b308-9a2c216aa58f","subject":"member","email":"me@example.com","idToken":"initial-id","scopes":[DIRECT]},"accountId":"member","email":"me@example.com","futureField":"preserved"})
}
fn response(access: &str, refresh: &str) -> Value {
    json!({"access_token":access,"refresh_token":refresh,"expires_in":3600,"scope":SCOPES})
}
fn endpoints() -> Endpoints {
    Endpoints {
        callback: "127.0.0.1:0".into(),
        ..Endpoints::default()
    }
}
async fn visit(url: &str, pairs: &[(&str, &str)]) -> String {
    let mut url = url::Url::parse(url).unwrap();
    url.query_pairs_mut().extend_pairs(pairs.iter().copied());
    let mut socket = tokio::net::TcpStream::connect(url.socket_addrs(|| None).unwrap()[0])
        .await
        .unwrap();
    socket
        .write_all(
            format!(
                "GET {}?{} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n",
                url.path(),
                url.query().unwrap_or("")
            )
            .as_bytes(),
        )
        .await
        .unwrap();
    let mut bytes = Vec::new();
    socket.read_to_end(&mut bytes).await.unwrap();
    String::from_utf8(bytes).unwrap()
}
#[test]
fn machine_identity_is_stable_private_and_shared_by_concurrent_logins() {
    let directory = Directory::new();
    let handles = (0..8)
        .map(|_| {
            let path = directory.0.clone();
            std::thread::spawn(move || host_id(&path).unwrap())
        })
        .collect::<Vec<_>>();
    let ids = handles
        .into_iter()
        .map(|t| t.join().unwrap())
        .collect::<Vec<_>>();
    assert!(ids.iter().all(|id| id == &ids[0]));
    assert!(ids[0].starts_with("urn:uuid:"));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(directory.0.join("chatgpt-api-host"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }
    assert_eq!(std::fs::read_dir(&directory.0).unwrap().count(), 1);
}
#[tokio::test]
async fn dynamic_client_signin_binds_state_nonce_pkce_resource_and_account_identity() {
    let directory = Directory::new();
    let client = Client::new(vec![]);
    let (start, flow) = authorize(&endpoints(), &directory.0, client.clone())
        .await
        .unwrap();
    let url = url::Url::parse(start["url"].as_str().unwrap()).unwrap();
    let query = url.query_pairs().into_owned().collect::<BTreeMap<_, _>>();
    for (key, want) in [
        ("client_id", "dynamic_agent_client"),
        ("agent_name_hint", "Nora"),
        ("response_type", "code"),
        ("resource", BASE),
        ("scope", SCOPES),
        ("code_challenge_method", "S256"),
    ] {
        assert_eq!(query[key], want);
    }
    assert_eq!(query["ext_agent_host_id"], host_id(&directory.0).unwrap());
    let id = jwt(
        json!({"iss":ISSUER,"aud":["oaiapp_registered"],"nonce":query["nonce"],"sub":"member","email":"me@example.com","exp":now()/1000+3600,"https://api.openai.com/auth":{"chatgpt_plan_type":"plus"}}),
    );
    let mut token = response("access-one", "refresh-one");
    token["id_token"] = json!(id);
    client.push(200, token);
    assert!(visit(
        &query["redirect_uri"],
        &[
            ("state", "someone-else"),
            ("code", "bad"),
            ("client_id", "other")
        ]
    )
    .await
    .starts_with("HTTP/1.1 400"));
    assert!(client.requests.lock().unwrap().is_empty());
    let pairs = [
        ("state", query["state"].as_str()),
        ("code", "the-code"),
        ("client_id", "oaiapp_registered"),
    ];
    let (browser, credentials) =
        tokio::join!(visit(&query["redirect_uri"], &pairs), callback(&flow));
    assert!(browser.contains("You're signed in"));
    assert!(!browser.contains("access-one"));
    let saved = credentials.unwrap();
    assert_eq!(saved["plan"], "plus");
    assert_eq!(saved["email"], "me@example.com");
    assert_eq!(
        saved["accountIdentity"],
        json!({"issuer":ISSUER,"subject":"member","scope":"chatgpt-api"})
    );
    let form = client.form(0);
    for (key, want) in [
        ("grant_type", "authorization_code"),
        ("client_id", "oaiapp_registered"),
        ("code", "the-code"),
        ("resource", BASE),
    ] {
        assert_eq!(form[key], want);
    }
    assert_eq!(
        URL_SAFE_NO_PAD.encode(Sha256::digest(form["code_verifier"].as_bytes())),
        query["code_challenge"]
    );
    assert!(!form.contains_key("client_secret"));
    assert_eq!(form["redirect_uri"], query["redirect_uri"]);
}
#[tokio::test]
async fn incomplete_registration_scope_and_mismatched_id_claims_save_nothing() {
    for case in [
        "client", "scope", "nonce", "issuer", "audience", "expired", "no-id",
    ] {
        let directory = Directory::new();
        let client = Client::new(vec![]);
        let (start, flow) = authorize(&endpoints(), &directory.0, client.clone())
            .await
            .unwrap();
        let query = url::Url::parse(start["url"].as_str().unwrap())
            .unwrap()
            .query_pairs()
            .into_owned()
            .collect::<BTreeMap<_, _>>();
        let mut claims = json!({"iss":ISSUER,"aud":"oaiapp_registered","nonce":query["nonce"],"sub":"member","exp":now()/1000+3600});
        match case {
            "nonce" => claims["nonce"] = json!("other"),
            "issuer" => claims["iss"] = json!("https://evil.test"),
            "audience" => claims["aud"] = json!("other-client"),
            "expired" => claims["exp"] = json!(now() / 1000 - 120),
            _ => (),
        }
        let mut token = response("access-one", "refresh-one");
        token["id_token"] = json!(if case == "no-id" {
            String::new()
        } else {
            jwt(claims)
        });
        if case == "scope" {
            token["scope"] = json!("openid profile email");
        }
        if case != "client" {
            client.push(200, token);
        }
        let client_id = if case == "client" {
            ""
        } else {
            "oaiapp_registered"
        };
        let pairs = [
            ("state", query["state"].as_str()),
            ("code", "code"),
            ("client_id", client_id),
        ];
        let (_, result) = tokio::join!(visit(&query["redirect_uri"], &pairs), callback(&flow));
        assert!(result.is_err(), "{case}");
        assert_eq!(
            client.requests.lock().unwrap().len(),
            usize::from(case != "client")
        );
    }
}
#[tokio::test]
async fn cancellation_releases_callback_socket_without_exchanging_a_code() {
    let directory = Directory::new();
    let client = Client::new(vec![]);
    let (start, flow) = authorize(&endpoints(), &directory.0, client.clone())
        .await
        .unwrap();
    let query = url::Url::parse(start["url"].as_str().unwrap())
        .unwrap()
        .query_pairs()
        .into_owned()
        .collect::<BTreeMap<_, _>>();
    let redirect = url::Url::parse(&query["redirect_uri"]).unwrap();
    drop(flow);
    tokio::task::yield_now().await;
    assert!(
        tokio::net::TcpStream::connect(redirect.socket_addrs(|| None).unwrap()[0])
            .await
            .is_err()
    );
    assert!(client.requests.lock().unwrap().is_empty());
}
#[tokio::test]
async fn refresh_uses_issued_client_and_rotated_successor_for_all_stale_waiters() {
    let mut reply = Reply::json(200, response("access-two", "refresh-two"));
    reply.delay = Duration::from_millis(10);
    let client = Client::new(vec![reply]);
    let rotations = Rotations::default();
    let old = saved(60_000);
    let (one, two) = tokio::join!(
        rotations.refresh(&old, client.clone(), endpoints(), false),
        rotations.refresh(&old, client.clone(), endpoints(), false)
    );
    let one = one.unwrap();
    assert_eq!(one, two.unwrap());
    assert_eq!(one["refresh"], "refresh-two");
    assert_eq!(one["extras"]["idToken"], "initial-id");
    assert_eq!(one["futureField"], "preserved");
    let form = client.form(0);
    for (key, want) in [
        ("grant_type", "refresh_token"),
        ("client_id", "oaiapp_registered"),
        ("refresh_token", "refresh-one"),
        ("resource", BASE),
    ] {
        assert_eq!(form[key], want);
    }
    assert!(!form.contains_key("scope"));
    assert!(!form.contains_key("client_secret"));
    assert_eq!(
        rotations
            .refresh(&old, client.clone(), endpoints(), true)
            .await
            .unwrap(),
        one
    );
    assert_eq!(
        rotations
            .refresh(&one, client.clone(), endpoints(), false)
            .await
            .unwrap(),
        one
    );
    assert_eq!(client.requests.lock().unwrap().len(), 1);
    let mut next = response("access-three", "refresh-three");
    next["id_token"] = json!("new-id");
    client.push(200, next);
    let three = rotations
        .refresh(&one, client.clone(), endpoints(), true)
        .await
        .unwrap();
    assert_eq!(three["extras"]["idToken"], "new-id");
    client.push(200, response("access-four", "refresh-four"));
    let four = rotations
        .refresh(&three, client.clone(), endpoints(), true)
        .await
        .unwrap();
    assert_eq!(four["extras"]["idToken"], "new-id");
    assert_eq!(client.form(2)["refresh_token"], "refresh-three");
}
#[tokio::test]
async fn explicit_dead_refresh_codes_expire_accounts_but_temporary_errors_keep_credentials() {
    for code in [
        "invalid_grant",
        "invalid_refresh_token",
        "token_expired",
        "refresh_token_expired",
        "refresh_token_invalidated",
        "refresh_token_reused",
    ] {
        let client = Client::new(vec![Reply::json(
            400,
            json!({"error":code,"error_description":"private refresh-one"}),
        )]);
        let error = Rotations::default()
            .refresh(&saved(60_000), client, endpoints(), false)
            .await
            .unwrap_err();
        assert_eq!(error.code, "authentication_required");
        assert!(!error.message.contains("refresh-one"));
    }
    for status in [401, 403, 429, 500] {
        let old = saved(60_000);
        let client = Client::new(vec![Reply::raw(status, "private refresh-one")]);
        assert_eq!(
            Rotations::default()
                .refresh(&old, client, endpoints(), false)
                .await
                .unwrap(),
            old
        );
        let client = Client::new(vec![Reply::raw(status, "private refresh-one")]);
        let error = Rotations::default()
            .refresh(&old, client, endpoints(), true)
            .await
            .unwrap_err();
        assert_ne!(error.code, "authentication_required");
        assert!(!error.message.contains("refresh-one"));
    }
}
#[tokio::test]
async fn revocation_discovers_own_endpoint_uses_only_registered_client_and_never_refreshes() {
    let client = Client::new(vec![
        Reply::json(
            200,
            json!({"revocation_endpoint":format!("{ISSUER}/oauth/revoke")}),
        ),
        Reply::raw(200, ""),
    ]);
    revoke(client.as_ref(), &endpoints(), &saved(-1))
        .await
        .unwrap();
    let requests = client.requests.lock().unwrap();
    assert_eq!(requests.len(), 2);
    assert_eq!(
        requests[0].url,
        format!("{ISSUER}/.well-known/openid-configuration")
    );
    drop(requests);
    assert_eq!(
        client.form(1),
        BTreeMap::from([
            ("token".into(), "refresh-one".into()),
            ("token_type_hint".into(), "refresh_token".into()),
            ("client_id".into(), "oaiapp_registered".into())
        ])
    );
    for endpoint in [
        "https://evil.test/revoke",
        "https://user@auth.openai.com/revoke",
        "https://auth.openai.com/revoke#private",
    ] {
        let client = Client::new(vec![Reply::json(
            200,
            json!({"revocation_endpoint":endpoint}),
        )]);
        assert!(revoke(client.as_ref(), &endpoints(), &saved(1000))
            .await
            .is_err());
        assert_eq!(client.requests.lock().unwrap().len(), 1);
    }
}
