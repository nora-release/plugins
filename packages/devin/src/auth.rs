use super::{
    account::{self, Account},
    network::{self, Client},
};
use base64::Engine;
use nora_ai::{HttpMethod, HttpRequest};
use nora_provider_plugin_sdk::{PluginError, PluginResult};
use rand::RngCore;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::time::Duration;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    task::JoinHandle,
};
pub(crate) struct Pending {
    task: Option<JoinHandle<PluginResult<Account>>>,
}
impl Drop for Pending {
    fn drop(&mut self) {
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}
impl Pending {
    pub async fn complete(mut self) -> PluginResult<Account> {
        let result = self
            .task
            .as_mut()
            .ok_or_else(account::expired)?
            .await
            .map_err(|_| account::expired());
        self.task.take();
        result?
    }
}
fn random(bytes: usize) -> String {
    let mut value = vec![0u8; bytes];
    rand::thread_rng().fill_bytes(&mut value);
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(value)
}
pub(crate) async fn finish(
    client: &dyn nora_ai::HttpClient,
    key: String,
    server: String,
    source: Option<String>,
) -> PluginResult<Account> {
    let who = network::user(client, &key, &server).await?;
    let profile = account::identity(&who)?;
    Ok(Account {
        key,
        server,
        profile,
        source,
        saved: Value::Null,
    })
}
async fn exchange(
    client: &dyn nora_ai::HttpClient,
    code: &str,
    verifier: &str,
    redirect: &str,
) -> PluginResult<Account> {
    let request = HttpRequest::new(
        HttpMethod::Post,
        format!(
            "{}/exa.seat_management_pb.SeatManagementService/ExchangeDevinCLIPKCECode",
            network::SERVER
        ),
    )
    .with_header("content-type", "application/json")
    .with_header("accept", "application/json")
    .with_body(
        json!({"code":code,"code_verifier":verifier,"redirect_uri":redirect})
            .to_string()
            .into_bytes(),
    );
    let value = network::json(client, request).await?;
    let key = account::text(
        value
            .get("sessionToken")
            .or_else(|| value.get("session_token")),
        64 * 1024,
    )
    .filter(|s| !s.chars().any(char::is_whitespace))
    .ok_or_else(|| PluginError::transport("Devin returned no session token."))?;
    finish(client, key.into(), network::SERVER.into(), None).await
}
pub(crate) async fn start(client: Client) -> PluginResult<(Pending, String)> {
    let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
        .await
        .map_err(|_| PluginError::invalid("The local Devin callback port could not be opened."))?;
    let port = listener
        .local_addr()
        .map_err(|_| account::expired())?
        .port();
    let redirect = format!("http://127.0.0.1:{port}/callback");
    let state = random(24);
    let verifier = random(48);
    let challenge = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .encode(Sha256::digest(verifier.as_bytes()));
    let mut url = url::Url::parse("https://app.devin.ai/auth/cli/continue").unwrap();
    url.query_pairs_mut()
        .append_pair("redirect_uri", &redirect)
        .append_pair("state", &state)
        .append_pair("prompt", "select_account")
        .append_pair("code_challenge", &challenge)
        .append_pair("code_challenge_method", "S256")
        .append_pair("cli_pkce_marker", "1");
    let task = tokio::spawn(async move {
        tokio::time::timeout(Duration::from_secs(600),async move{for _ in 0..64{let(mut socket,peer)=listener.accept().await.map_err(|_|account::expired())?;if !peer.ip().is_loopback(){continue;}let mut bytes=Vec::new();loop{let mut buffer=[0u8;4096];let n=tokio::time::timeout(Duration::from_secs(5),socket.read(&mut buffer)).await.map_err(|_|account::expired())?.map_err(|_|account::expired())?;if n==0{break;}bytes.extend_from_slice(&buffer[..n]);if bytes.len()>16384{return Err(account::expired());}if bytes.windows(4).any(|w|w==b"\r\n\r\n"){break;}}let line=std::str::from_utf8(&bytes).unwrap_or_default().lines().next().unwrap_or_default();let mut parts=line.split_whitespace();let method=parts.next().unwrap_or_default();let target=parts.next().unwrap_or_default();let parsed=if method=="GET"&&target.starts_with('/')&&!target.starts_with("//"){url::Url::parse(&format!("http://127.0.0.1{target}")).ok()}else{None};let query=parsed.as_ref().filter(|u|u.path()=="/callback").map(|u|u.query_pairs().into_owned().collect::<std::collections::BTreeMap<_,_>>());let Some(query)=query.filter(|q|q.get("state")==Some(&state))else{let _=socket.write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").await;continue;};let result=if query.contains_key("error"){Err(account::expired())}else if let Some(code)=query.get("code").filter(|s|!s.is_empty()&&s.len()<=16384){exchange(client.as_ref(),code,&verifier,&redirect).await}else{continue;};let body=if result.is_ok(){"Signed in to Devin. You can close this tab."}else{"Devin sign-in did not finish. Return to Nora and try again."};let response=format!("HTTP/1.1 200 OK\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len());let _=socket.write_all(response.as_bytes()).await;return result;}Err(account::expired())}).await.map_err(|_|account::expired())?
    });
    Ok((Pending { task: Some(task) }, url.to_string()))
}
#[cfg(test)]
#[path = "../test/auth.test.rs"]
mod tests;
