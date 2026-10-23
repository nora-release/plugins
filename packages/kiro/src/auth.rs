use super::{
    network,
    session::{self, Credential},
};
use base64::Engine;
use nora_ai::HttpClient;
use nora_provider_plugin_sdk::{PluginError, PluginResult};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::sync::Arc;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
};
const PORTS: [u16; 10] = [
    3128, 4649, 6588, 8008, 9091, 49153, 50153, 51153, 52153, 53153,
];
const SCOPES: [&str; 5] = [
    "codewhisperer:completions",
    "codewhisperer:analysis",
    "codewhisperer:conversations",
    "codewhisperer:transformations",
    "codewhisperer:taskassist",
];
pub(crate) struct Pending {
    task: Option<tokio::task::JoinHandle<PluginResult<Credential>>>,
    pub client: Arc<dyn HttpClient>,
}
impl Drop for Pending {
    fn drop(&mut self) {
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}
impl Pending {
    pub async fn complete(mut self) -> PluginResult<Credential> {
        self.task
            .take()
            .ok_or_else(session::expired)?
            .await
            .map_err(|_| session::expired())?
    }
}
fn random() -> String {
    nora_ai::oauth_pkce::random_hex(24)
}
fn challenge(verifier: &str) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}
struct Aws {
    state: String,
    verifier: String,
    region: String,
    client_id: String,
    client_secret: String,
    builder: bool,
    realm: String,
}
pub(crate) async fn start(
    client: Arc<dyn HttpClient>,
    installation: &str,
) -> PluginResult<(Pending, String)> {
    let mut chosen = None;
    for port in PORTS {
        if let Ok(listener) = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, port)).await {
            chosen = Some(listener);
            break;
        }
    }
    let listener =
        chosen.ok_or_else(|| PluginError::transport("Kiro sign-in callback ports are in use."))?;
    start_listener(client, listener, installation)
}
fn start_listener(
    client: Arc<dyn HttpClient>,
    listener: TcpListener,
    installation: &str,
) -> PluginResult<(Pending, String)> {
    let port = listener
        .local_addr()
        .map_err(|_| session::expired())?
        .port();
    let state = random();
    let verifier = nora_ai::oauth_pkce::random_hex(48);
    let redirect = format!("http://localhost:{port}");
    let mut url = url::Url::parse("https://app.kiro.dev/signin").unwrap();
    url.query_pairs_mut()
        .append_pair("state", &state)
        .append_pair("code_challenge", &challenge(&verifier))
        .append_pair("code_challenge_method", "S256")
        .append_pair("redirect_uri", &redirect)
        .append_pair("redirect_from", "KiroIDE");
    let outgoing = url.to_string();
    let client_clone = client.clone();
    let ua = format!(
        "KiroIDE-1.1.70-{}",
        session::fingerprint(&format!("nora:{installation}"))
    );
    let task = tokio::spawn(async move {
        tokio::time::timeout(
            std::time::Duration::from_secs(600),
            listen(listener, port, state, verifier, client_clone.as_ref(), &ua),
        )
        .await
        .map_err(|_| PluginError::authentication("Kiro sign-in timed out."))?
    });
    Ok((
        Pending {
            task: Some(task),
            client,
        },
        outgoing,
    ))
}
async fn answer(socket: &mut TcpStream, status: &str, location: Option<&str>) {
    let text = if location.is_some() {
        "Continue Kiro sign-in in your browser."
    } else {
        "Return to Nora to finish Kiro sign-in."
    };
    let response=format!("HTTP/1.1 {status}\r\nContent-Type: text/plain; charset=utf-8\r\nCache-Control: no-store\r\nConnection: close\r\nContent-Length: {}\r\n{}\r\n{text}",text.len(),location.map(|url|format!("Location: {url}\r\n")).unwrap_or_default());
    let _ = socket.write_all(response.as_bytes()).await;
    let _ = socket.shutdown().await;
}
async fn callback(socket: &mut TcpStream) -> PluginResult<url::Url> {
    let mut bytes = Vec::new();
    loop {
        let mut chunk = [0; 1024];
        let size = tokio::time::timeout(std::time::Duration::from_secs(5), socket.read(&mut chunk))
            .await
            .map_err(|_| session::expired())?
            .map_err(|_| session::expired())?;
        if size == 0 {
            return Err(session::expired());
        }
        bytes.extend_from_slice(&chunk[..size]);
        if bytes.len() > 16 * 1024 {
            return Err(session::expired());
        }
        if bytes.windows(4).any(|window| window == b"\r\n\r\n") {
            break;
        }
    }
    let request = std::str::from_utf8(&bytes).map_err(|_| session::expired())?;
    let mut parts = request
        .lines()
        .next()
        .unwrap_or_default()
        .split_whitespace();
    if parts.next() != Some("GET") {
        return Err(session::expired());
    }
    let path = parts
        .next()
        .filter(|path| path.starts_with('/') && !path.starts_with("//"))
        .ok_or_else(session::expired)?;
    let url =
        url::Url::parse(&format!("http://localhost{path}")).map_err(|_| session::expired())?;
    if !["/oauth/callback", "/signin/callback"].contains(&url.path()) {
        return Err(session::expired());
    }
    Ok(url)
}
async fn signin(
    client: &dyn HttpClient,
    url: String,
    body: &Value,
    ua: &str,
) -> PluginResult<Value> {
    network::json(
        client,
        network::post(url, body)?.with_header("user-agent", ua),
    )
    .await
}
fn tokens(value: &Value, method: &str, region: &str) -> PluginResult<Credential> {
    let mut result = Credential {
        access: session::secret(value.get("accessToken"))?,
        refresh: session::text(value.get("refreshToken"), 64 * 1024)
            .unwrap_or_default()
            .into(),
        expires: session::now()
            + value["expiresIn"]
                .as_i64()
                .filter(|seconds| *seconds > 0 && *seconds < 31_536_000)
                .unwrap_or(3600)
                * 1000,
        method: method.into(),
        region: region.into(),
        profile: session::text(value.get("profileArn"), 2048)
            .unwrap_or_default()
            .into(),
        ..Default::default()
    };
    if result.refresh.is_empty() {
        return Err(PluginError::authentication(
            "Kiro did not return a renewable sign-in.",
        ));
    }
    if method == "social" {
        result.client_id.clear();
    }
    Ok(result)
}
async fn listen(
    listener: TcpListener,
    port: u16,
    state: String,
    verifier: String,
    client: &dyn HttpClient,
    ua: &str,
) -> PluginResult<Credential> {
    let mut aws: Option<Aws> = None;
    loop {
        let (mut socket, _) = listener.accept().await.map_err(|_| session::expired())?;
        let url = match callback(&mut socket).await {
            Ok(url) => url,
            Err(_) => {
                answer(&mut socket, "400 Bad Request", None).await;
                continue;
            }
        };
        let query = url
            .query_pairs()
            .collect::<std::collections::BTreeMap<_, _>>();
        let received = query
            .get("state")
            .map(|value| value.as_ref())
            .unwrap_or_default();
        let is_aws = aws.as_ref().is_some_and(|aws| received == aws.state);
        if received != state && !is_aws {
            answer(&mut socket, "400 Bad Request", None).await;
            continue;
        }
        if query.contains_key("error") {
            answer(&mut socket, "400 Bad Request", None).await;
            return Err(PluginError::authentication("Kiro sign-in was declined."));
        }
        let result:PluginResult<Option<Credential>>=async{
if is_aws{let aws=aws.take().unwrap();let code=query.get("code").filter(|value|!value.is_empty()).ok_or_else(session::expired)?;let value=signin(client,format!("https://oidc.{}.amazonaws.com/token",aws.region),&json!({"clientId":aws.client_id,"clientSecret":aws.client_secret,"grantType":"authorization_code","redirectUri":format!("http://127.0.0.1:{port}/oauth/callback"),"code":code,"codeVerifier":aws.verifier}),ua).await?;let mut credential=tokens(&value,"idc",&aws.region)?;credential.client_id=aws.client_id;credential.client_secret=aws.client_secret;credential.builder=aws.builder;credential.realm=aws.realm;return Ok(Some(credential));}
if aws.is_some(){return Err(session::expired());}let option=query.get("login_option").map(|value|value.as_ref()).unwrap_or_default();match option{
"google"|"github"=>{let code=query.get("code").filter(|value|!value.is_empty()).ok_or_else(session::expired)?;let value=signin(client,"https://prod.us-east-1.auth.desktop.kiro.dev/oauth/token".into(),&json!({"code":code,"code_verifier":verifier,"redirect_uri":format!("http://localhost:{port}{}?login_option={option}",url.path())}),ua).await?;Ok(Some(tokens(&value,"social","us-east-1")?))},
"builderid"|"awsidc"|"internal"=>{let region=session::region(query.get("idc_region").map(|value|value.as_ref()).unwrap_or_default())?;let issuer=query.get("issuer_url").ok_or_else(session::expired)?;let parsed=url::Url::parse(issuer).map_err(|_|session::expired())?;if parsed.scheme()!="https"||parsed.host_str().is_none()||!parsed.username().is_empty()||parsed.password().is_some()||parsed.fragment().is_some(){return Err(PluginError::invalid("Kiro returned an invalid IAM Identity Center issuer."));}let registration=signin(client,format!("https://oidc.{region}.amazonaws.com/client/register"),&json!({"clientName":"Kiro IDE","clientType":"public","scopes":SCOPES,"grantTypes":["authorization_code","refresh_token"],"redirectUris":["http://127.0.0.1/oauth/callback"],"issuerUrl":issuer}),ua).await?;let next=Aws{state:random(),verifier:nora_ai::oauth_pkce::random_hex(48),region:region.clone(),client_id:session::secret(registration.get("clientId"))?,client_secret:session::secret(registration.get("clientSecret"))?,builder:option=="builderid",realm:if option=="builderid"{String::new()}else{session::realm(issuer)?}};let mut authorize=url::Url::parse(&format!("https://oidc.{region}.amazonaws.com/authorize")).unwrap();authorize.query_pairs_mut().append_pair("response_type","code").append_pair("client_id",&next.client_id).append_pair("redirect_uri",&format!("http://127.0.0.1:{port}/oauth/callback")).append_pair("scopes",&SCOPES.join(",")).append_pair("state",&next.state).append_pair("code_challenge",&challenge(&next.verifier)).append_pair("code_challenge_method","S256");aws=Some(next);answer(&mut socket,"302 Found",Some(authorize.as_str())).await;Ok(None)},
"external_idp"=>Err(PluginError::unsupported("company identity browser sign-in; select a Kiro CLI source instead")),_=>Err(PluginError::authentication("Kiro returned an unsupported sign-in method.")),}
}.await;
        match result {
            Ok(None) => continue,
            Ok(Some(mut credential)) => match network::identify(client, &mut credential).await {
                Ok(_) => {
                    answer(&mut socket, "200 OK", None).await;
                    return Ok(credential);
                }
                Err(error) => {
                    answer(&mut socket, "400 Bad Request", None).await;
                    return Err(error);
                }
            },
            Err(error) => {
                answer(&mut socket, "400 Bad Request", None).await;
                return Err(error);
            }
        }
    }
}
#[cfg(test)]
#[path = "../test/auth.test.rs"]
mod tests;
