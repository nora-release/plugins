use super::*;
use async_trait::async_trait;
use jsonwebtoken::{EncodingKey, Header};
use nora_ai::http::{HttpClientError, HttpStreamingResponse};
use rand::rngs::OsRng;
use rsa::pkcs1::EncodeRsaPrivateKey;
use rsa::traits::PublicKeyParts;
use rsa::RsaPrivateKey;
use std::collections::{BTreeMap, VecDeque};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Default)]
struct StubHttpClient {
    responses: Mutex<VecDeque<HttpResponse>>,
    requests: Mutex<Vec<HttpRequest>>,
}

impl StubHttpClient {
    fn push_json(&self, status: u16, value: Value) {
        self.responses.lock().unwrap().push_back(HttpResponse {
            status,
            headers: BTreeMap::new(),
            body: serde_json::to_vec(&value).unwrap(),
        });
    }
}

#[async_trait]
impl HttpClient for StubHttpClient {
    async fn stream(
        &self,
        _request: HttpRequest,
    ) -> Result<HttpStreamingResponse, HttpClientError> {
        unreachable!("xAI OIDC tests use one-shot requests")
    }

    async fn request(&self, request: HttpRequest) -> Result<HttpResponse, HttpClientError> {
        self.requests.lock().unwrap().push(request);
        self.responses
            .lock()
            .unwrap()
            .pop_front()
            .ok_or_else(|| HttpClientError::Transport("missing stub response".to_string()))
    }
}

struct HangingHttpClient;

#[async_trait]
impl HttpClient for HangingHttpClient {
    async fn stream(
        &self,
        _request: HttpRequest,
    ) -> Result<HttpStreamingResponse, HttpClientError> {
        unreachable!("timeout test uses a one-shot request")
    }

    async fn request(&self, _request: HttpRequest) -> Result<HttpResponse, HttpClientError> {
        std::future::pending().await
    }
}

struct SignedToken {
    token: String,
    jwks: Value,
}

fn signed_token(mut claims: XaiIdTokenClaims) -> SignedToken {
    let private_key = RsaPrivateKey::new(&mut OsRng, 2048).unwrap();
    let der = private_key.to_pkcs1_der().unwrap();
    let mut header = Header::new(Algorithm::RS256);
    header.kid = Some("test-key".to_string());
    if claims.exp == 0 {
        claims.exp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs()
            + 3_600;
    }
    let token =
        jsonwebtoken::encode(&header, &claims, &EncodingKey::from_rsa_der(der.as_bytes())).unwrap();
    let jwks = serde_json::json!({
        "keys": [{
            "kty": "RSA",
            "kid": "test-key",
            "alg": "RS256",
            "use": "sig",
            "n": URL_SAFE_NO_PAD.encode(private_key.n().to_bytes_be()),
            "e": URL_SAFE_NO_PAD.encode(private_key.e().to_bytes_be()),
        }]
    });
    SignedToken { token, jwks }
}

fn discovery() -> XaiOidcDiscovery {
    XaiOidcDiscovery {
        issuer: XAI_OIDC_ISSUER.to_string(),
        authorization_endpoint: "https://auth.x.ai/oauth2/authorize".to_string(),
        token_endpoint: "https://auth.x.ai/oauth2/token".to_string(),
        jwks_uri: "https://auth.x.ai/oauth2/jwks".to_string(),
        id_token_signing_alg_values_supported: vec!["RS256".to_string()],
    }
}

fn valid_claims() -> XaiIdTokenClaims {
    XaiIdTokenClaims {
        sub: Some("user-123".to_string()),
        email: Some("nora@example.com".to_string()),
        name: Some("Nora User".to_string()),
        iss: Some(XAI_OIDC_ISSUER.to_string()),
        aud: Some(Value::String("xai-client".to_string())),
        nonce: Some("nonce-123".to_string()),
        first_name: Some("Nora".to_string()),
        ..XaiIdTokenClaims::default()
    }
}

#[tokio::test]
async fn validates_signed_xai_id_token() {
    let client = StubHttpClient::default();
    let signed = signed_token(valid_claims());
    client.push_json(200, signed.jwks);

    let user = validate_xai_id_token(
        &client,
        &signed.token,
        &discovery(),
        "xai-client",
        "nonce-123",
    )
    .await
    .unwrap();

    assert_eq!(user.user_id, "user-123");
    assert_eq!(user.email.as_deref(), Some("nora@example.com"));
    assert_eq!(user.name.as_deref(), Some("Nora User"));
}

#[tokio::test]
async fn rejects_nonce_audience_and_expiry_mismatches() {
    for (claims, client_id, nonce) in [
        (valid_claims(), "xai-client", "wrong-nonce"),
        (valid_claims(), "wrong-client", "nonce-123"),
        (
            XaiIdTokenClaims {
                exp: 1,
                ..valid_claims()
            },
            "xai-client",
            "nonce-123",
        ),
    ] {
        let client = StubHttpClient::default();
        let signed = signed_token(claims);
        client.push_json(200, signed.jwks);
        assert!(
            validate_xai_id_token(&client, &signed.token, &discovery(), client_id, nonce,)
                .await
                .is_err()
        );
    }
}

#[tokio::test]
async fn rejects_bad_signature_kid_algorithm_and_issuer() {
    let signed = signed_token(valid_claims());
    let different_key = signed_token(valid_claims());
    let client = StubHttpClient::default();
    client.push_json(200, different_key.jwks);
    assert!(validate_xai_id_token(
        &client,
        &signed.token,
        &discovery(),
        "xai-client",
        "nonce-123",
    )
    .await
    .is_err());

    let signed = signed_token(valid_claims());
    let mut wrong_kid_jwks = signed.jwks;
    wrong_kid_jwks["keys"][0]["kid"] = Value::String("other-key".to_string());
    let client = StubHttpClient::default();
    client.push_json(200, wrong_kid_jwks);
    assert!(validate_xai_id_token(
        &client,
        &signed.token,
        &discovery(),
        "xai-client",
        "nonce-123",
    )
    .await
    .is_err());

    let signed = signed_token(XaiIdTokenClaims {
        iss: Some("https://attacker.example".to_string()),
        ..valid_claims()
    });
    let client = StubHttpClient::default();
    client.push_json(200, signed.jwks);
    assert!(validate_xai_id_token(
        &client,
        &signed.token,
        &discovery(),
        "xai-client",
        "nonce-123",
    )
    .await
    .is_err());

    let mut header = Header::new(Algorithm::HS256);
    header.kid = Some("symmetric-key".to_string());
    let token = jsonwebtoken::encode(
        &header,
        &valid_claims(),
        &EncodingKey::from_secret(b"not-allowed"),
    )
    .unwrap();
    let client = StubHttpClient::default();
    assert!(
        validate_xai_id_token(&client, &token, &discovery(), "xai-client", "nonce-123",)
            .await
            .is_err()
    );
}

#[tokio::test]
async fn team_principal_can_login_without_an_id_token() {
    let client = StubHttpClient::default();
    client.push_json(503, serde_json::json!({"error": "temporarily unavailable"}));
    let payload = URL_SAFE_NO_PAD.encode(
        serde_json::to_vec(&serde_json::json!({
            "principal_type": "Team",
            "principal_id": "team-123"
        }))
        .unwrap(),
    );
    let tokens = OAuthTokenResponse {
        access_token: format!("e30.{payload}.signature"),
        refresh_token: Some("refresh".to_string()),
        expires_in: 3_600,
        scope: None,
        id_token: None,
    };

    let user =
        resolve_xai_login_identity(&client, &tokens, &discovery(), "xai-client", "nonce-123")
            .await
            .unwrap();

    assert_eq!(user.user_id, "team-123");
    assert_eq!(user.principal_type.as_deref(), Some("Team"));
    assert_eq!(user.team_id.as_deref(), Some("team-123"));
}

#[tokio::test]
async fn discovery_rejects_a_different_issuer() {
    let client = StubHttpClient::default();
    client.push_json(
        200,
        serde_json::json!({
            "issuer": "https://attacker.example",
            "authorization_endpoint": "https://attacker.example/authorize",
            "token_endpoint": "https://attacker.example/token",
            "jwks_uri": "https://attacker.example/jwks"
        }),
    );

    let error = discover_xai_oidc(&client).await.unwrap_err();

    assert!(error.to_string().contains("issuer mismatch"));
    assert_eq!(
        client.requests.lock().unwrap()[0].url,
        "https://auth.x.ai/.well-known/openid-configuration"
    );
}

#[tokio::test]
async fn oidc_requests_have_a_bounded_timeout() {
    let error = oidc_request_with_timeout(
        &HangingHttpClient,
        HttpRequest::new(HttpMethod::Get, "https://example.test"),
        "xAI OIDC test request",
        Duration::from_millis(1),
    )
    .await
    .unwrap_err();

    assert_eq!(
        error,
        OAuthError::Transport("xAI OIDC test request timed out".to_string())
    );
}
