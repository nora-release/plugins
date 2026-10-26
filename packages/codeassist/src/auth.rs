use super::*;
use nora_ai::{HttpClient, HttpMethod, HttpRequest, OAuthPkce};
use nora_provider_auth::oauth_callback_server::OAuthCallbackServer;
use std::time::Duration;

pub(crate) struct Pending {
    pub server: OAuthCallbackServer,
    pub pkce: OAuthPkce,
    pub state: String,
    pub redirect: String,
    pub project: String,
    pub client: std::sync::Arc<dyn HttpClient>,
}

pub(crate) async fn start(
    app: App,
    project: String,
    client: std::sync::Arc<dyn HttpClient>,
) -> PluginResult<(Pending, String)> {
    let path = if app == App::Antigravity {
        "/oauth-callback"
    } else {
        "/oauth2callback"
    };
    let host = if app == App::Antigravity {
        "localhost"
    } else {
        "127.0.0.1"
    };
    let mut server = OAuthCallbackServer::with_redirect_host(0, path, host);
    server.success_html =
        "<!doctype html><title>Nora</title><p>Google sign-in received. Return to Nora.</p>".into();
    server.error_html =
        "<!doctype html><title>Nora</title><p>Google sign-in did not finish. Return to Nora.</p>"
            .into();
    server
        .start()
        .await
        .map_err(|_| invalid("The Google sign-in callback could not be opened."))?;
    let redirect = server.redirect_uri();
    let pkce = OAuthPkce::random();
    let state = nora_ai::oauth_pkce::random_hex(24);
    let mut url =
        url::Url::parse("https://accounts.google.com/o/oauth2/v2/auth").expect("constant URL");
    url.query_pairs_mut().extend_pairs([
        ("client_id", app.client_id()),
        ("redirect_uri", redirect.as_str()),
        ("response_type", "code"),
        ("scope", app.scopes().as_str()),
        ("access_type", "offline"),
        ("prompt", "consent"),
        ("state", state.as_str()),
        ("code_challenge", pkce.challenge.as_str()),
        ("code_challenge_method", "S256"),
    ]);
    Ok((
        Pending {
            server,
            pkce,
            state,
            redirect,
            project,
            client,
        },
        url.into(),
    ))
}

pub(crate) async fn complete(mut pending: Pending, app: App) -> PluginResult<session::Tokens> {
    let fields = tokio::time::timeout(Duration::from_secs(590), pending.server.wait_for_callback())
        .await
        .map_err(|_| expired())?
        .map_err(|_| expired())?;
    if fields.get("state") != Some(&pending.state) || fields.contains_key("error") {
        return Err(expired());
    }
    let code = text(
        fields
            .get("code")
            .map(|value| Value::String(value.clone()))
            .as_ref(),
        8192,
    )
    .ok_or_else(expired)?
    .to_owned();
    let payload = exchange(
        app,
        &[
            ("grant_type", "authorization_code"),
            ("code", &code),
            ("code_verifier", &pending.pkce.verifier),
            ("redirect_uri", &pending.redirect),
        ],
        pending.client.as_ref(),
    )
    .await?;
    let mut tokens = read_tokens(&payload, None)?;
    tokens.project = pending.project;
    identify(&mut tokens, pending.client.as_ref()).await?;
    Ok(tokens)
}

pub(crate) async fn exchange(
    app: App,
    fields: &[(&str, &str)],
    client: &dyn HttpClient,
) -> PluginResult<Value> {
    nora_provider_plugin_sdk::require_subscription_accounts()?;
    let body = {
        let mut form = url::form_urlencoded::Serializer::new(String::new());
        form.extend_pairs(fields.iter().copied());
        form.extend_pairs([
            ("client_id", app.client_id()),
            ("client_secret", app.client_secret()),
        ]);
        form.finish()
    };
    json_request(
        client,
        HttpRequest::new(HttpMethod::Post, TOKEN)
            .with_header("content-type", "application/x-www-form-urlencoded")
            .with_body(body),
    )
    .await
}

pub(crate) fn read_tokens(
    payload: &Value,
    previous: Option<&session::Tokens>,
) -> PluginResult<session::Tokens> {
    let access = token(payload.get("access_token"))?;
    let refresh = match payload.get("refresh_token") {
        Some(value) => token(Some(value))?,
        None => previous
            .map(|tokens| tokens.refresh.clone())
            .ok_or_else(expired)?,
    };
    let seconds = payload
        .get("expires_in")
        .and_then(Value::as_i64)
        .filter(|seconds| *seconds > 0 && *seconds <= 31_536_000)
        .ok_or_else(expired)?;
    let mut tokens = previous.cloned().unwrap_or_default();
    tokens.access = access;
    tokens.refresh = refresh;
    tokens.expires = now().saturating_add(seconds.saturating_mul(1000));
    Ok(tokens)
}

pub(crate) async fn identify(
    tokens: &mut session::Tokens,
    client: &dyn HttpClient,
) -> PluginResult<()> {
    identify_with_identity(tokens, client).await.map(|_| ())
}

pub(crate) async fn identify_with_identity(
    tokens: &mut session::Tokens,
    client: &dyn HttpClient,
) -> PluginResult<Value> {
    let user = json_request(
        client,
        HttpRequest::new(HttpMethod::Get, USER_INFO)
            .with_header("authorization", format!("Bearer {}", tokens.access)),
    )
    .await?;
    if user.get("verified_email") == Some(&json!(false)) {
        return Err(expired());
    }
    tokens.email = text(user.get("email"), 512).ok_or_else(expired)?.into();
    tokens.uid = fingerprint(&format!(
        "google-account\0{}",
        text(user.get("id"), 512)
            .unwrap_or(&tokens.email)
            .to_lowercase()
    ));
    tokens.name = text(user.get("name"), 512).unwrap_or_default().into();
    let identity = match text(user.get("id"), 512) {
        Some(id) => json!({"issuer":"https://accounts.google.com","subject":id}),
        None => json!({"issuer":"urn:nora:google-email","subject":tokens.email.to_lowercase()}),
    };
    tokens.identity = Some(identity.clone());
    Ok(identity)
}

pub(crate) async fn json_request(
    client: &dyn HttpClient,
    request: HttpRequest,
) -> PluginResult<Value> {
    tokio::time::timeout(Duration::from_secs(25), async {
        let login_endpoint = request.url == TOKEN || request.url == USER_INFO;
        let response = client
            .stream(request)
            .await
            .map_err(|_| PluginError::transport("The Google service could not be reached."))?;
        if response.status == 401 || (response.status == 403 && login_endpoint) {
            return Err(expired());
        }
        if !(200..300).contains(&response.status) {
            return Err(PluginError::transport(format!(
                "The Google service returned HTTP {}.",
                response.status
            )));
        }
        let text = nora_ai::collect_http_body_text_limited(response.body, 4 * 1024 * 1024 + 1)
            .await
            .map_err(|_| {
                PluginError::transport("The Google response stopped before it finished.")
            })?;
        if text.len() > 4 * 1024 * 1024 {
            return Err(PluginError::transport("The Google response is too large."));
        }
        serde_json::from_str(&text)
            .map_err(|_| PluginError::transport("The Google service returned invalid JSON."))
    })
    .await
    .map_err(|_| PluginError::transport("The Google request timed out."))?
}

pub(crate) fn authorized(
    app: App,
    tokens: &session::Tokens,
    base: &str,
    method: &str,
    body: &Value,
) -> PluginResult<HttpRequest> {
    if ![PROD, app.base()].contains(&base)
        || !method.bytes().all(|byte| byte.is_ascii_alphanumeric())
    {
        return Err(invalid("The Code Assist endpoint is invalid."));
    }
    HttpRequest::new(HttpMethod::Post, format!("{base}/v1internal:{method}"))
        .with_header("authorization", format!("Bearer {}", tokens.access))
        .with_header(
            "user-agent",
            app.user_agent_with_version(
                "",
                if tokens.version.is_empty() {
                    "2.9.1"
                } else {
                    &tokens.version
                },
            ),
        )
        .with_json_body(body)
        .map_err(|_| invalid("The Code Assist request cannot be encoded."))
}

#[cfg(test)]
#[path = "../test/auth.test.rs"]
mod tests;
