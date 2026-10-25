use super::*;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::oneshot,
};

pub(super) async fn begin(
    provider: &Autoclaw,
    client: Arc<dyn HttpClient>,
    vendor: &str,
    ports: &[u16],
    lifetime: Duration,
) -> PluginResult<Value> {
    let mut listener = None;
    for port in ports {
        if let Ok(bound) = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, *port)).await {
            listener = Some(bound);
            break;
        }
    }
    let listener=listener.ok_or_else(||invalid("AutoClaw's registered callback ports are busy. Close its desktop sign-in window and try again."))?;
    let port = listener
        .local_addr()
        .map_err(|_| invalid("The AutoClaw callback address is unavailable."))?
        .port();
    let (sender, result) = oneshot::channel();
    let vendor = vendor.to_owned();
    let task = tokio::spawn(async move {
        let value = tokio::time::timeout(lifetime, listen(listener, client, &vendor, port))
            .await
            .unwrap_or_else(|_| {
                Err(invalid(
                    "The AutoClaw sign-in expired. Begin a new sign-in.",
                ))
            });
        let _ = sender.send(value);
    });
    let session = auth::insert(
        provider,
        auth::Pending {
            deadline: now() + auth::SESSION_MS,
            flow: auth::Flow::Browser {
                result,
                task: task.abort_handle(),
            },
        },
    )
    .await?;
    Ok(
        json!({"session":session,"url":format!("http://127.0.0.1:{port}/"),"method":"auto","instructions":"Complete the security check and sign in to AutoClaw in your browser. Closing the guide before authorization cancels this sign-in."}),
    )
}

struct Request {
    method: String,
    path: String,
    headers: BTreeMap<String, String>,
    body: Vec<u8>,
}
async fn read(socket: &mut TcpStream) -> PluginResult<Request> {
    let mut bytes = Vec::new();
    let (end, length) = loop {
        let mut buffer = [0u8; 4096];
        let n = socket
            .read(&mut buffer)
            .await
            .map_err(|_| invalid("The AutoClaw guide connection stopped."))?;
        if n == 0 {
            return Err(invalid("The AutoClaw guide request is incomplete."));
        }
        bytes.extend_from_slice(&buffer[..n]);
        if bytes.len() > 65536 {
            return Err(invalid("The AutoClaw guide request is too large."));
        }
        if let Some(end) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
            let head = std::str::from_utf8(&bytes[..end])
                .map_err(|_| invalid("The AutoClaw guide request is invalid."))?;
            let length = head
                .lines()
                .filter_map(|l| l.split_once(':'))
                .find(|(k, _)| k.eq_ignore_ascii_case("content-length"))
                .map(|(_, v)| v.trim().parse::<usize>())
                .transpose()
                .map_err(|_| invalid("The AutoClaw guide body length is invalid."))?
                .unwrap_or(0);
            if length > 32768 {
                return Err(invalid("The AutoClaw guide request is too large."));
            }
            break (end, length);
        }
    };
    while bytes.len() < end + 4 + length {
        let mut buffer = [0u8; 4096];
        let n = socket
            .read(&mut buffer)
            .await
            .map_err(|_| invalid("The AutoClaw guide connection stopped."))?;
        if n == 0 {
            return Err(invalid("The AutoClaw guide request is incomplete."));
        }
        bytes.extend_from_slice(&buffer[..n]);
        if bytes.len() > 65536 {
            return Err(invalid("The AutoClaw guide request is too large."));
        }
    }
    let head = std::str::from_utf8(&bytes[..end])
        .map_err(|_| invalid("The AutoClaw guide request is invalid."))?;
    let mut lines = head.lines();
    let first = lines
        .next()
        .unwrap_or_default()
        .split_whitespace()
        .collect::<Vec<_>>();
    if first.len() != 3 || !first[1].starts_with('/') || first[1].starts_with("//") {
        return Err(invalid("The AutoClaw guide target is invalid."));
    }
    let headers = lines
        .filter_map(|line| line.split_once(':'))
        .map(|(k, v)| (k.to_ascii_lowercase(), v.trim().to_owned()))
        .collect::<BTreeMap<_, _>>();
    if headers.contains_key("transfer-encoding") {
        return Err(invalid(
            "The AutoClaw guide request encoding is unsupported.",
        ));
    }
    Ok(Request {
        method: first[0].into(),
        path: first[1].into(),
        headers,
        body: bytes[end + 4..end + 4 + length].to_vec(),
    })
}
async fn reply(
    socket: &mut TcpStream,
    status: u16,
    kind: &str,
    body: &str,
    location: Option<&str>,
) {
    let redirect = location
        .map(|v| format!("Location: {v}\r\n"))
        .unwrap_or_default();
    let head=format!("HTTP/1.1 {status} Reply\r\nContent-Type: {kind}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\nX-Content-Type-Options: nosniff\r\nReferrer-Policy: no-referrer\r\n{redirect}\r\n",body.len());
    let _ = socket.write_all(head.as_bytes()).await;
    let _ = socket.write_all(body.as_bytes()).await;
    let _ = socket.shutdown().await;
}
fn local(request: &Request, port: u16) -> bool {
    let allowed = [format!("127.0.0.1:{port}"), format!("localhost:{port}")];
    if !request
        .headers
        .get("host")
        .is_some_and(|host| allowed.contains(host))
    {
        return false;
    }
    request.headers.get("origin").is_none_or(|origin| {
        url::Url::parse(origin).is_ok_and(|u| {
            u.scheme() == "http"
                && u.port() == Some(port)
                && matches!(u.host_str(), Some("127.0.0.1" | "localhost"))
                && u.username().is_empty()
                && u.password().is_none()
        })
    })
}
fn form(request: &Request) -> Value {
    if request
        .headers
        .get("content-type")
        .is_some_and(|v| v.starts_with("application/json"))
    {
        return serde_json::from_slice(&request.body).unwrap_or(Value::Null);
    }
    Value::Object(
        url::form_urlencoded::parse(&request.body)
            .map(|(k, v)| (k.into_owned(), json!(v)))
            .collect(),
    )
}
async fn listen(
    listener: TcpListener,
    client: Arc<dyn HttpClient>,
    vendor: &str,
    port: u16,
) -> PluginResult<Value> {
    let device = random_hex(32)?;
    let mut receipt = String::new();
    let mut expected_state = None;
    loop {
        let (mut socket, peer) = listener
            .accept()
            .await
            .map_err(|_| invalid("The AutoClaw guide stopped."))?;
        if !peer.ip().is_loopback() {
            continue;
        }
        let request = tokio::time::timeout(Duration::from_secs(5), read(&mut socket)).await;
        let Ok(Ok(request)) = request else {
            reply(
                &mut socket,
                400,
                "text/plain",
                "Invalid local request",
                None,
            )
            .await;
            continue;
        };
        let Ok(url) = url::Url::parse(&format!("http://127.0.0.1:{port}{}", request.path)) else {
            reply(
                &mut socket,
                400,
                "text/plain",
                "Invalid local request",
                None,
            )
            .await;
            continue;
        };
        if !local(&request, port) {
            reply(
                &mut socket,
                403,
                "text/plain",
                "Forbidden local origin",
                None,
            )
            .await;
            continue;
        }
        let input = form(&request);
        match (request.method.as_str(), url.path()) {
            ("GET", "/" | "/index.html") => {
                let config = transport::call(
                    client.as_ref(),
                    Site::International.host(),
                    "/userapi/overseasv1/oauth-captcha-config",
                    HttpMethod::Post,
                    "",
                    Some(json!({})),
                )
                .await;
                let config = match config {
                    Ok(r) if r.code == 0 => r.data,
                    _ => {
                        reply(&mut socket,502,"text/html",&failure("AutoClaw could not load the security check. Try opening the sign-in again."),None).await;
                        continue;
                    }
                };
                reply(
                    &mut socket,
                    200,
                    "text/html; charset=utf-8",
                    &html(vendor, &config),
                    None,
                )
                .await;
            }
            ("POST", "/captcha") => {
                receipt = text(&input, "receipt").trim().to_owned();
                reply(&mut socket, 200, "application/json", "{\"ok\":true}", None).await;
            }
            ("POST", "/open-auth") => {
                if !text(&input, "receipt").is_empty() {
                    receipt = text(&input, "receipt").to_owned();
                }
                let redirect = format!("http://localhost:{port}/auth/callback-{vendor}");
                let mut body =
                    json!({"source_id":"autoclaw","device_id":device,"navigate_uri":redirect});
                if !receipt.is_empty() {
                    body["ali_captcha_verify_param"] = json!(receipt);
                }
                let response = transport::call(
                    client.as_ref(),
                    Site::International.host(),
                    &format!("/userapi/overseasv1/{vendor}-oauth-url"),
                    HttpMethod::Post,
                    "",
                    Some(body),
                )
                .await;
                let authorization = response
                    .ok()
                    .filter(|r| r.code == 0)
                    .and_then(|r| {
                        ["oauth_url", "url", "login_url", "auth_url", "verify_url"]
                            .into_iter()
                            .find_map(|key| {
                                r.data.get(key).and_then(Value::as_str).map(str::to_string)
                            })
                    })
                    .and_then(|s| url::Url::parse(&s).ok())
                    .filter(|u| {
                        u.scheme() == "https"
                            && u.host_str().is_some()
                            && u.username().is_empty()
                            && u.password().is_none()
                    });
                let Some(authorization) = authorization else {
                    reply(
                        &mut socket,
                        502,
                        "text/html",
                        &failure(
                            "AutoClaw did not return an authorization page. Try again from Nora.",
                        ),
                        None,
                    )
                    .await;
                    continue;
                };
                let state = authorization
                    .query_pairs()
                    .find(|(k, _)| k == "state")
                    .map(|(_, v)| v.into_owned())
                    .filter(|s| {
                        !s.is_empty() && s.len() <= 16384 && !s.chars().any(char::is_control)
                    });
                let Some(state) = state else {
                    reply(
                        &mut socket,
                        502,
                        "text/html",
                        &failure(
                            "AutoClaw did not return an authorization state. Begin a new sign-in.",
                        ),
                        None,
                    )
                    .await;
                    continue;
                };
                expected_state = Some(state);
                reply(
                    &mut socket,
                    303,
                    "text/plain",
                    "Continue to AutoClaw authorization",
                    Some(authorization.as_str()),
                )
                .await;
            }
            ("GET", path) if path == format!("/auth/callback-{vendor}") => {
                let query = url.query_pairs().into_owned().collect::<BTreeMap<_, _>>();
                let valid = expected_state
                    .as_ref()
                    .is_some_and(|state| query.get("state") == Some(state))
                    && query.get("code").is_some_and(|code| {
                        !code.is_empty()
                            && code.len() <= 16384
                            && !code.chars().any(char::is_control)
                    });
                if !valid {
                    reply(&mut socket,400,"text/html",&failure("This callback was rejected because its state did not match. Return to the original sign-in page."),None).await;
                    continue;
                }
                reply(&mut socket,200,"text/html","<!doctype html><meta charset=utf-8><title>AutoClaw</title><p>Authorization received. Return to Nora to finish signing in.</p>",None).await;
                let response=transport::call(client.as_ref(),Site::International.host(),&format!("/userapi/overseasv1/{vendor}-oauth-login"),HttpMethod::Post,"",Some(json!({"source_id":"autoclaw","device_id":device,"code":query["code"],"state":query["state"],"navigate_uri":format!("http://localhost:{port}/auth/callback-{vendor}")}))).await?;
                if response.code != 0 {
                    return Err(signed_out());
                }
                let mut credentials =
                    auth::tokens(&response.data, &Value::Null, Site::International, now())?;
                auth::profile(
                    client.as_ref(),
                    Site::International,
                    &mut credentials,
                    if vendor == "zai" {
                        "z.ai account"
                    } else {
                        "Google account"
                    },
                )
                .await;
                return Ok(credentials);
            }
            ("POST", "/abort") => {
                reply(
                    &mut socket,
                    200,
                    "text/html; charset=utf-8",
                    "<!doctype html><meta charset=utf-8><title>AutoClaw</title><p>Sign-in cancelled. You can close this tab.</p>",
                    None,
                )
                .await;
                return Err(invalid("The AutoClaw sign-in was cancelled."));
            }
            _ => reply(&mut socket, 404, "text/plain", "Not found", None).await,
        }
    }
}
fn failure(message: &str) -> String {
    format!("<!doctype html><meta charset=utf-8><title>AutoClaw sign-in</title><p>{message}</p><p><a href='/'>Return to the sign-in guide</a></p>")
}
fn html(vendor: &str, config: &Value) -> String {
    let name = if vendor == "zai" { "z.ai" } else { "Google" };
    let enabled = config["enabled"] == true;
    let mut captcha = String::new();
    if enabled {
        let field = |key: &str, default: &str| {
            config
                .get(key)
                .and_then(Value::as_str)
                .filter(|s| {
                    !s.is_empty()
                        && s.len() <= 128
                        && s.bytes()
                            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
                })
                .unwrap_or(default)
                .to_owned()
        };
        let scene = field("scene_id", "");
        let region = field("region", "ga");
        let prefix = field("prefix", "sq51tr");
        // This is the vendor CAPTCHA SDK's page bridge only. Authorization
        // URLs, state verification and token exchange remain in Rust.
        captcha = format!(
            r##"<div id="captchaEle"></div><button type="button" id="captchaTrigger" disabled>Complete security check</button>
<script>window.AliyunCaptchaConfig={{region:"{region}",prefix:"{prefix}"}};</script>
<script src="https://o.alicdn.com/captcha-frontend/aliyunCaptcha/AliyunCaptcha.js"></script>
<script>window.initAliyunCaptcha({{SceneId:"{scene}",mode:"popup",element:"#captchaEle",button:"#captchaTrigger",language:"cn",slideStyle:{{width:320,height:40}},captchaVerifyCallback:(receipt)=>{{document.getElementById("receipt").value=typeof receipt==="string"?receipt:(receipt.captchaVerifyParam||JSON.stringify(receipt));document.getElementById("goBtn").disabled=false;return {{captchaResult:true}};}},onBizResultCallback:()=>{{}},getInstance:()=>{{}}}});setTimeout(()=>{{document.getElementById("captchaTrigger").disabled=false}},2300);</script>"##
        );
    }
    format!(
        r##"<!doctype html><html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1"><title>AutoClaw · {name}</title><style>body{{font:15px -apple-system,sans-serif;background:#f5f6fa;max-width:460px;margin:50px auto;padding:24px}}main{{background:white;border-radius:14px;padding:28px}}button{{padding:10px 18px;margin:12px 0}}</style></head><body><main><h1>AutoClaw · {name}</h1><p>Complete the security check, then continue to sign in. Return to Nora after authorization.</p><form id="authorize" method="POST" action="/open-auth"><input type="hidden" name="receipt" id="receipt">{captcha}<button id="goBtn" {}>Continue authorization</button></form><form method="POST" action="/abort"><button>Cancel sign-in</button></form><p>The local sign-in expires after ten minutes.</p></main>
<script>let formNavigation=false;document.querySelectorAll("form").forEach(form=>form.addEventListener("submit",()=>{{formNavigation=true}}));window.addEventListener("beforeunload",()=>{{if(!formNavigation)navigator.sendBeacon("/abort",new Blob(["{{}}"],{{type:"application/json"}}));}});</script></body></html>"##,
        if enabled { "disabled" } else { "" }
    )
}

#[cfg(test)]
#[path = "../test/guide.test.rs"]
mod tests;
