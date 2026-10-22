use crate::xai_auth::{xai_oauth_user_request, XaiOAuthUser};
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use jsonwebtoken::{Algorithm, DecodingKey, Validation};
use nora_ai::http::{HttpClient, HttpMethod, HttpRequest, HttpResponse};
use nora_ai::oauth::{OAuthError, OAuthTokenResponse};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::time::Duration;

pub const XAI_OIDC_ISSUER: &str = "https://auth.x.ai";
pub const XAI_TEAM_PRINCIPAL_TYPE: &str = "Team";

const ALLOWED_ID_TOKEN_ALGORITHMS: &[Algorithm] = &[
    Algorithm::RS256,
    Algorithm::RS384,
    Algorithm::RS512,
    Algorithm::PS256,
    Algorithm::PS384,
    Algorithm::PS512,
    Algorithm::ES256,
    Algorithm::ES384,
    Algorithm::EdDSA,
];
const XAI_OIDC_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const XAI_USER_ENRICHMENT_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Clone, Debug, Deserialize)]
pub struct XaiOidcDiscovery {
    pub issuer: String,
    pub authorization_endpoint: String,
    pub token_endpoint: String,
    pub jwks_uri: String,
    #[serde(default)]
    pub id_token_signing_alg_values_supported: Vec<String>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
struct XaiIdTokenClaims {
    #[serde(default)]
    sub: Option<String>,
    #[serde(default)]
    email: Option<String>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    iss: Option<String>,
    #[serde(default)]
    aud: Option<Value>,
    #[serde(default)]
    nonce: Option<String>,
    #[serde(default, alias = "given_name")]
    first_name: Option<String>,
    #[serde(default, alias = "family_name")]
    last_name: Option<String>,
    #[serde(default)]
    picture: Option<String>,
    #[serde(default)]
    exp: u64,
}

#[derive(Deserialize)]
struct XaiAccessTokenPrincipal {
    #[serde(default, alias = "principalType")]
    principal_type: Option<String>,
    #[serde(default, alias = "principalId")]
    principal_id: Option<String>,
    #[serde(default, alias = "teamId")]
    team_id: Option<String>,
}

pub async fn discover_xai_oidc(client: &dyn HttpClient) -> Result<XaiOidcDiscovery, OAuthError> {
    let url = format!(
        "{}/.well-known/openid-configuration",
        XAI_OIDC_ISSUER.trim_end_matches('/')
    );
    let response = oidc_request_with_timeout(
        client,
        HttpRequest::new(HttpMethod::Get, url).with_header("accept", "application/json"),
        "xAI OIDC discovery",
        XAI_OIDC_REQUEST_TIMEOUT,
    )
    .await?;
    ensure_success("xAI OIDC discovery", &response)?;
    let discovery =
        serde_json::from_slice::<XaiOidcDiscovery>(&response.body).map_err(|error| {
            OAuthError::InvalidResponse(format!("could not decode xAI OIDC discovery: {error}"))
        })?;
    if discovery.issuer.trim_end_matches('/') != XAI_OIDC_ISSUER {
        return Err(OAuthError::InvalidResponse(
            "xAI OIDC discovery issuer mismatch".to_string(),
        ));
    }
    for (name, endpoint) in [
        ("authorization", discovery.authorization_endpoint.as_str()),
        ("token", discovery.token_endpoint.as_str()),
        ("JWKS", discovery.jwks_uri.as_str()),
    ] {
        let valid = url::Url::parse(endpoint)
            .ok()
            .is_some_and(|url| url.scheme() == "https" && url.host_str().is_some());
        if !valid {
            return Err(OAuthError::InvalidResponse(format!(
                "xAI OIDC discovery returned an invalid {name} endpoint"
            )));
        }
    }
    Ok(discovery)
}

pub async fn resolve_xai_login_identity(
    client: &dyn HttpClient,
    tokens: &OAuthTokenResponse,
    discovery: &XaiOidcDiscovery,
    client_id: &str,
    nonce: &str,
) -> Result<XaiOAuthUser, OAuthError> {
    let identity = match access_token_principal(&tokens.access_token) {
        Some(principal) if principal.principal_type.as_deref() == Some(XAI_TEAM_PRINCIPAL_TYPE) => {
            let principal_id = normalized(principal.principal_id).ok_or_else(|| {
                OAuthError::InvalidResponse("xAI team access token missing principalId".to_string())
            })?;
            XaiOAuthUser {
                user_id: principal_id.clone(),
                principal_type: Some(XAI_TEAM_PRINCIPAL_TYPE.to_string()),
                principal_id: Some(principal_id.clone()),
                team_id: normalized(principal.team_id).or(Some(principal_id)),
                ..XaiOAuthUser::default()
            }
        }
        _ => {
            let id_token = tokens.id_token.as_deref().ok_or_else(|| {
                OAuthError::InvalidResponse("xAI token response missing id_token".to_string())
            })?;
            validate_xai_id_token(client, id_token, discovery, client_id, nonce).await?
        }
    };

    enrich_xai_login_identity(client, &tokens.access_token, identity).await
}

pub async fn validate_xai_id_token(
    client: &dyn HttpClient,
    token: &str,
    discovery: &XaiOidcDiscovery,
    client_id: &str,
    nonce: &str,
) -> Result<XaiOAuthUser, OAuthError> {
    let header = jsonwebtoken::decode_header(token).map_err(|error| {
        OAuthError::InvalidResponse(format!("could not decode xAI id_token header: {error}"))
    })?;
    let kid = header.kid.ok_or_else(|| {
        OAuthError::InvalidResponse("xAI id_token header missing kid".to_string())
    })?;
    ensure_algorithm_allowed(header.alg, &discovery.id_token_signing_alg_values_supported)?;

    let response = oidc_request_with_timeout(
        client,
        HttpRequest::new(HttpMethod::Get, discovery.jwks_uri.clone())
            .with_header("accept", "application/json"),
        "xAI OIDC JWKS",
        XAI_OIDC_REQUEST_TIMEOUT,
    )
    .await?;
    ensure_success("xAI OIDC JWKS", &response)?;
    let jwks =
        serde_json::from_slice::<jsonwebtoken::jwk::JwkSet>(&response.body).map_err(|error| {
            OAuthError::InvalidResponse(format!("could not decode xAI JWKS: {error}"))
        })?;
    let jwk = jwks.find(&kid).ok_or_else(|| {
        OAuthError::InvalidResponse(format!("xAI JWKS did not contain key `{kid}`"))
    })?;
    let decoding_key = DecodingKey::from_jwk(jwk).map_err(|error| {
        OAuthError::InvalidResponse(format!("could not construct xAI JWT key: {error}"))
    })?;

    let mut validation = Validation::new(header.alg);
    validation.set_issuer(&[XAI_OIDC_ISSUER]);
    validation.set_audience(&[client_id]);
    validation.validate_exp = true;
    validation.validate_aud = true;
    validation.required_spec_claims = ["sub", "iss", "aud", "exp"]
        .into_iter()
        .map(ToOwned::to_owned)
        .collect();
    let claims = jsonwebtoken::decode::<XaiIdTokenClaims>(token, &decoding_key, &validation)
        .map_err(|error| {
            OAuthError::InvalidResponse(format!("xAI id_token validation failed: {error}"))
        })?
        .claims;

    if claims.iss.as_deref().map(str::trim) != Some(XAI_OIDC_ISSUER) {
        return Err(OAuthError::InvalidResponse(
            "xAI id_token issuer mismatch".to_string(),
        ));
    }
    if !claims
        .aud
        .as_ref()
        .is_some_and(|audience| audience_matches(audience, client_id))
    {
        return Err(OAuthError::InvalidResponse(
            "xAI id_token audience mismatch".to_string(),
        ));
    }
    if claims.nonce.as_deref() != Some(nonce) {
        return Err(OAuthError::InvalidResponse(
            "xAI id_token nonce mismatch".to_string(),
        ));
    }
    let user_id = normalized(claims.sub)
        .ok_or_else(|| OAuthError::InvalidResponse("xAI id_token missing subject".to_string()))?;
    Ok(XaiOAuthUser {
        user_id,
        email: normalized(claims.email),
        name: normalized(claims.name),
        first_name: normalized(claims.first_name),
        last_name: normalized(claims.last_name),
        profile_image_asset_id: normalized(claims.picture),
        ..XaiOAuthUser::default()
    })
}

async fn enrich_xai_login_identity(
    client: &dyn HttpClient,
    access_token: &str,
    fallback: XaiOAuthUser,
) -> Result<XaiOAuthUser, OAuthError> {
    let response = match oidc_request_with_timeout(
        client,
        xai_oauth_user_request(access_token)?,
        "xAI account profile",
        XAI_USER_ENRICHMENT_TIMEOUT,
    )
    .await
    {
        Ok(response) => response,
        Err(_) => return Ok(fallback),
    };
    if matches!(response.status, 401 | 403) {
        return Err(OAuthError::RefreshFailed(
            "xAI account proxy rejected the OAuth session".to_string(),
        ));
    }
    if response.status >= 400 {
        return Ok(fallback);
    }
    let mut user = match serde_json::from_slice::<XaiOAuthUser>(&response.body) {
        Ok(user) => user,
        Err(_) => return Ok(fallback),
    };
    if user.user_id.trim().is_empty() {
        user.user_id = fallback.user_id.clone();
    }
    fill_if_missing(&mut user.email, fallback.email);
    fill_if_missing(&mut user.name, fallback.name);
    fill_if_missing(&mut user.first_name, fallback.first_name);
    fill_if_missing(&mut user.last_name, fallback.last_name);
    fill_if_missing(
        &mut user.profile_image_asset_id,
        fallback.profile_image_asset_id,
    );
    fill_if_missing(&mut user.principal_type, fallback.principal_type);
    fill_if_missing(&mut user.principal_id, fallback.principal_id);
    fill_if_missing(&mut user.team_id, fallback.team_id);
    Ok(user)
}

async fn oidc_request_with_timeout(
    client: &dyn HttpClient,
    request: HttpRequest,
    operation: &str,
    timeout: Duration,
) -> Result<HttpResponse, OAuthError> {
    tokio::time::timeout(timeout, client.request(request))
        .await
        .map_err(|_| OAuthError::Transport(format!("{operation} timed out")))?
        .map_err(OAuthError::from)
}

fn access_token_principal(token: &str) -> Option<XaiAccessTokenPrincipal> {
    let payload = token.split('.').nth(1)?;
    let payload = URL_SAFE_NO_PAD.decode(payload).ok()?;
    serde_json::from_slice(&payload).ok()
}

fn audience_matches(audience: &Value, expected: &str) -> bool {
    match audience {
        Value::String(value) => value == expected,
        Value::Array(values) => values.iter().any(|value| value.as_str() == Some(expected)),
        _ => false,
    }
}

fn ensure_algorithm_allowed(
    algorithm: Algorithm,
    discovery_algorithms: &[String],
) -> Result<(), OAuthError> {
    let name = algorithm_name(algorithm);
    if !ALLOWED_ID_TOKEN_ALGORITHMS.contains(&algorithm) {
        return Err(OAuthError::InvalidResponse(format!(
            "xAI id_token uses unsupported algorithm {name}"
        )));
    }
    if !discovery_algorithms.is_empty()
        && !discovery_algorithms
            .iter()
            .any(|candidate| candidate == name)
    {
        return Err(OAuthError::InvalidResponse(format!(
            "xAI id_token algorithm {name} is not allowed by discovery"
        )));
    }
    Ok(())
}

fn algorithm_name(algorithm: Algorithm) -> &'static str {
    match algorithm {
        Algorithm::HS256 => "HS256",
        Algorithm::HS384 => "HS384",
        Algorithm::HS512 => "HS512",
        Algorithm::ES256 => "ES256",
        Algorithm::ES384 => "ES384",
        Algorithm::RS256 => "RS256",
        Algorithm::RS384 => "RS384",
        Algorithm::RS512 => "RS512",
        Algorithm::PS256 => "PS256",
        Algorithm::PS384 => "PS384",
        Algorithm::PS512 => "PS512",
        Algorithm::EdDSA => "EdDSA",
    }
}

fn ensure_success(operation: &str, response: &HttpResponse) -> Result<(), OAuthError> {
    if response.status < 400 {
        return Ok(());
    }
    Err(OAuthError::InvalidResponse(format!(
        "{operation} returned status {}",
        response.status
    )))
}

fn normalized(value: Option<String>) -> Option<String> {
    value
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn fill_if_missing(target: &mut Option<String>, fallback: Option<String>) {
    if target
        .as_deref()
        .map(str::trim)
        .unwrap_or_default()
        .is_empty()
    {
        *target = fallback;
    }
}

#[cfg(test)]
mod tests {
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
            jsonwebtoken::encode(&header, &claims, &EncodingKey::from_rsa_der(der.as_bytes()))
                .unwrap();
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
}
