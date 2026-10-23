use super::{
    account::{self, Account},
    network::Client,
};
use base64::Engine;
use nora_provider_plugin_sdk::{PluginError, PluginResult};
use rsa::{pkcs1::EncodeRsaPublicKey, Oaep, Pkcs1v15Encrypt, RsaPrivateKey, RsaPublicKey};
use serde_json::json;
use std::time::Duration;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};
pub(crate) struct Pending {
    task: Option<tokio::task::JoinHandle<PluginResult<Account>>>,
    pub client: Client,
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
        self.task
            .take()
            .ok_or_else(account::expired)?
            .await
            .map_err(|_| account::expired())?
    }
}
pub(crate) fn decrypt(key: &RsaPrivateKey, ciphertext: &str) -> PluginResult<String> {
    if ciphertext.len() > 4096 {
        return Err(account::expired());
    }
    let ciphertext = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(ciphertext.trim_end_matches('='))
        .map_err(|_| account::expired())?;
    let plain = key
        .decrypt_blinded(
            &mut rand::rngs::OsRng,
            Oaep::new::<sha2::Sha256>(),
            &ciphertext,
        )
        .or_else(|_| key.decrypt_blinded(&mut rand::rngs::OsRng, Pkcs1v15Encrypt, &ciphertext))
        .map_err(|_| account::expired())?;
    let token = String::from_utf8(plain).map_err(|_| account::expired())?;
    if token.is_empty()
        || token.len() > 64 * 1024
        || token.chars().any(char::is_whitespace)
        || token.chars().any(char::is_control)
    {
        return Err(account::expired());
    }
    Ok(token)
}
pub(crate) async fn start(client: Client) -> PluginResult<(Pending, String)> {
    let key = tokio::task::spawn_blocking(|| RsaPrivateKey::new(&mut rand::rngs::OsRng, 2048))
        .await
        .map_err(|_| account::expired())?
        .map_err(|_| PluginError::transport("Zed sign-in encryption could not be initialized."))?;
    let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
        .await
        .map_err(|_| {
            PluginError::transport("Zed sign-in callback could not listen on localhost.")
        })?;
    start_listener(client, key, listener)
}
fn start_listener(
    client: Client,
    key: RsaPrivateKey,
    listener: TcpListener,
) -> PluginResult<(Pending, String)> {
    let port = listener
        .local_addr()
        .map_err(|_| account::expired())?
        .port();
    let public = RsaPublicKey::from(&key)
        .to_pkcs1_der()
        .map_err(|_| account::expired())?;
    let public = base64::engine::general_purpose::URL_SAFE.encode(public.as_bytes());
    let system = account::uuid();
    let mut url = url::Url::parse("https://zed.dev/native_app_signin").unwrap();
    url.query_pairs_mut()
        .append_pair("native_app_port", &port.to_string())
        .append_pair("native_app_public_key", &public)
        .append_pair("system_id", &system);
    let task = tokio::spawn(async move {
        tokio::time::timeout(Duration::from_secs(600),async move{
        for _ in 0..64{
            let(mut socket,peer)=listener.accept().await.map_err(|_|account::expired())?;if !peer.ip().is_loopback(){continue;}
            let mut bytes=Vec::new();let mut buffer=[0u8;4096];loop{let n=tokio::time::timeout(Duration::from_secs(5),socket.read(&mut buffer)).await.map_err(|_|account::expired())?.map_err(|_|account::expired())?;if n==0{break;}bytes.extend_from_slice(&buffer[..n]);if bytes.len()>32*1024{return Err(account::expired());}if bytes.windows(4).any(|s|s==b"\r\n\r\n"){break;}}
            let request=std::str::from_utf8(&bytes).unwrap_or_default().lines().next().unwrap_or_default();let mut request=request.split_whitespace();let method=request.next().unwrap_or_default();let target=request.next().unwrap_or_default();
            let result=if method=="GET"{url::Url::parse(&format!("http://127.0.0.1{target}")).ok().and_then(|url|{let q:std::collections::BTreeMap<_,_>=url.query_pairs().into_owned().collect();let user=q.get("user_id")?;let token=decrypt(&key,q.get("access_token")?).ok()?;Account::parse(&json!({"userID":user,"access":token,"systemID":system,"authMethod":"browser"})).ok()})}else{None};
            let Some(account)=result else{let _=socket.write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").await;continue;};
            let _=socket.write_all(b"HTTP/1.1 302 Found\r\nLocation: https://zed.dev/native_app_signin_succeeded\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").await;return Ok(account);
        }Err(account::expired())
    }).await.map_err(|_|PluginError::authentication("Zed sign-in timed out."))?
    });
    Ok((
        Pending {
            task: Some(task),
            client,
        },
        url.to_string(),
    ))
}
#[cfg(test)]
#[path = "../test/auth.test.rs"]
mod tests;
