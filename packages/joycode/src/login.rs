use crate::*;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    task::{JoinHandle, JoinSet},
};
pub(crate) struct Flow {
    created: std::time::Instant,
    task: JoinHandle<PluginResult<Value>>,
}
impl Drop for Flow {
    fn drop(&mut self) {
        self.task.abort();
    }
}
pub(crate) async fn authorize(provider: &JoyCode, params: Value) -> PluginResult<Value> {
    let method = params
        .get("authMethodID")
        .or_else(|| params.get("method"))
        .and_then(Value::as_str)
        .unwrap_or("oauth");
    let fields = params
        .get("inputs")
        .or_else(|| params.get("fields"))
        .unwrap_or(&params);
    let client = provider.params_client(&params)?;
    if matches!(method, "api" | "apiKey") {
        let key = fields
            .get("apiKey")
            .or_else(|| fields.get("key"))
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim();
        let credentials = json!({"type":"apiKey","key":key,"metadata":{"tenant":text(fields,"tenant"),"loginType":LOGIN_TYPE}});
        let user = query(client.as_ref(), &credentials, "joycode_userInfo", json!({})).await?;
        return Ok(
            json!({"ok":true,"credentials":credentials_for(key,LOGIN_TYPE,text(fields,"tenant"),&user)?}),
        );
    }
    if method != "oauth" {
        return Err(PluginError::unsupported(method));
    }
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .map_err(|_| transport("JoyCode could not open its sign-in callback."))?;
    let port = listener
        .local_addr()
        .map_err(|_| transport("JoyCode sign-in callback is unavailable."))?
        .port();
    let auth_key = nora_ai::oauth_pkce::random_hex(16);
    let session = nora_ai::oauth_pkce::random_hex(16);
    let mut url = url::Url::parse("https://joycode.jd.com/login/").unwrap();
    url.query_pairs_mut()
        .append_pair("ideAppName", "JoyCode")
        .append_pair("fromIde", "ide")
        .append_pair("redirect", "0")
        .append_pair("authPort", &port.to_string())
        .append_pair("authKey", &auth_key);
    let task = tokio::spawn(async move {
        tokio::time::timeout(Duration::from_secs(600),async move{
            let mut clients=JoinSet::new();
            loop {tokio::select! {
                accepted=listener.accept()=>{
                    let (stream,_)=accepted.map_err(|_|transport("JoyCode sign-in callback stopped."))?;
                    if clients.len()>=32 {drop(stream);continue;}
                    let auth_key=auth_key.clone();let client=client.clone();clients.spawn(async move{connection(stream,port,&auth_key,client.as_ref()).await});
                }
                result=clients.join_next(),if !clients.is_empty()=>{if let Some(Ok(Some(result)))=result {return result;}}
            }}
        }).await.map_err(|_|invalid("JoyCode sign-in timed out. Start again."))?
    });
    let mut pending = provider.pending.lock().await;
    pending.retain(|_, flow| flow.created.elapsed() < Duration::from_secs(600));
    if pending.len() >= 32 {
        task.abort();
        return Err(invalid("Too many JoyCode sign-ins are waiting."));
    }
    pending.insert(
        session.clone(),
        Flow {
            created: std::time::Instant::now(),
            task,
        },
    );
    Ok(
        json!({"session":session,"url":url.to_string(),"method":"auto","instructions":"Sign in with your JD account in the browser."}),
    )
}
pub(crate) async fn callback(provider: &JoyCode, params: Value) -> PluginResult<Value> {
    let mut flow = provider
        .pending
        .lock()
        .await
        .remove(text(&params, "session"))
        .ok_or_else(|| invalid("The JoyCode sign-in session expired."))?;
    let credentials = (&mut flow.task)
        .await
        .map_err(|_| transport("The JoyCode sign-in was cancelled."))??;
    Ok(json!({"ok":true,"credentials":credentials}))
}
async fn connection(
    mut stream: TcpStream,
    port: u16,
    key: &str,
    client: &dyn HttpClient,
) -> Option<PluginResult<Value>> {
    let mut bytes = Vec::new();
    let read = tokio::time::timeout(Duration::from_secs(5), async {
        let mut chunk = [0u8; 1024];
        while !bytes.windows(4).any(|end| end == b"\r\n\r\n") {
            let n = stream.read(&mut chunk).await?;
            if n == 0 || bytes.len() + n > 16384 {
                return Ok::<_, std::io::Error>(false);
            }
            bytes.extend_from_slice(&chunk[..n]);
        }
        Ok(true)
    })
    .await;
    if !matches!(read, Ok(Ok(true))) {
        return None;
    }
    let request = String::from_utf8_lossy(&bytes);
    let mut words = request.lines().next().unwrap_or("").split_whitespace();
    let method = words.next().unwrap_or("");
    let target = words.next().unwrap_or("");
    let url = target
        .strip_prefix('/')
        .and_then(|target| url::Url::parse(&format!("http://127.0.0.1:{port}/{target}")).ok());
    let Some(url) = url else {
        let _ = reply(&mut stream, 404, "Not found").await;
        return None;
    };
    let values = url.query_pairs().into_owned().collect::<BTreeMap<_, _>>();
    if method != "GET"
        || values.get("authKey").map(String::as_str) != Some(key)
        || values
            .get("authPort")
            .and_then(|port| port.parse::<u16>().ok())
            != Some(port)
    {
        let _ = reply(&mut stream, 404, "Not found").await;
        return None;
    }
    let key = values.get("pt_key").map(String::as_str).unwrap_or("");
    let kind = values
        .get("loginType")
        .map(String::as_str)
        .unwrap_or(LOGIN_TYPE);
    let tenant = values.get("tenant").map(String::as_str).unwrap_or("");
    let result = async {
        let credentials =
            json!({"type":"apiKey","key":key,"metadata":{"loginType":kind,"tenant":tenant}});
        let user = query_checked(client, &credentials, "joycode_userInfo", json!({}), true).await?;
        credentials_for(key, kind, tenant, &user)
    }
    .await;
    let html = if result.is_ok() {
        "<!doctype html><meta charset=utf-8><h2>JoyCode 登录成功</h2><p>可以返回 Nora。</p><script>setTimeout(()=>window.close(),2000)</script>"
    } else {
        "<!doctype html><meta charset=utf-8><h2>JoyCode 登录失败</h2><p>账号验证未通过，请返回 Nora 重新登录。</p>"
    };
    let _ = reply(&mut stream, 200, html).await;
    Some(result)
}
async fn reply(stream: &mut TcpStream, status: u16, body: &str) -> std::io::Result<()> {
    let wire=format!("HTTP/1.1 {status} {}\r\ncontent-type: text/html; charset=utf-8\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",if status==200{"OK"}else{"Not Found"},body.len());
    tokio::time::timeout(Duration::from_secs(3), stream.write_all(wire.as_bytes()))
        .await
        .map_err(|_| {
            std::io::Error::new(std::io::ErrorKind::TimedOut, "callback write timed out")
        })?
}
