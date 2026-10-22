use nora_provider_plugin_sdk::{NetworkProxy, PluginError, PluginResult};
use std::{collections::BTreeMap, sync::Arc};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    task::JoinSet,
};
const KEYS: [&str; 8] = [
    "HTTP_PROXY",
    "HTTPS_PROXY",
    "ALL_PROXY",
    "NO_PROXY",
    "http_proxy",
    "https_proxy",
    "all_proxy",
    "no_proxy",
];
fn failed() -> PluginError {
    PluginError::transport("The Claude Code network proxy could not connect.")
}
pub(crate) struct Proxy {
    pub env: BTreeMap<String, String>,
    task: Option<tokio::task::JoinHandle<()>>,
}
impl Drop for Proxy {
    fn drop(&mut self) {
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}
impl Proxy {
    pub async fn new(choice: Option<&NetworkProxy>) -> PluginResult<Arc<Self>> {
        if let Some(choice) = choice {
            choice.validate()?;
        }
        let selected = match choice {
            Some(NetworkProxy::Proxy { url }) => Some(url.as_str()),
            _ => None,
        };
        let mut task = None;
        let value = if let Some(selected) = selected {
            let url = url::Url::parse(selected).map_err(|_| failed())?;
            if ["socks5", "socks5h"].contains(&url.scheme()) {
                let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
                    .await
                    .map_err(|_| failed())?;
                let address = listener.local_addr().map_err(|_| failed())?;
                let value = format!("http://{address}");
                task = Some(tokio::spawn(async move {
                    let mut tasks = JoinSet::new();
                    loop {
                        tokio::select! {accepted=listener.accept()=>{let Ok((client,_))=accepted else{break;};let proxy=url.clone();tasks.spawn(async move{let _=relay(client,proxy).await;});},_=tasks.join_next(),if !tasks.is_empty()=>{}}
                    }
                }));
                Some(value)
            } else {
                Some(selected.into())
            }
        } else {
            None
        };
        let mut env = BTreeMap::new();
        for key in KEYS {
            let val = if key.eq_ignore_ascii_case("no_proxy") {
                if value.is_some() {
                    ""
                } else {
                    "*"
                }
            } else {
                value.as_deref().unwrap_or_default()
            };
            env.insert(key.into(), val.into());
        }
        Ok(Arc::new(Self { env, task }))
    }
}
fn decode(value: &str) -> PluginResult<Vec<u8>> {
    let mut out = Vec::new();
    let mut bytes = value.as_bytes().iter().copied();
    while let Some(byte) = bytes.next() {
        if byte == b'%' {
            let a = bytes
                .next()
                .and_then(|byte| (byte as char).to_digit(16))
                .ok_or_else(failed)?;
            let b = bytes
                .next()
                .and_then(|byte| (byte as char).to_digit(16))
                .ok_or_else(failed)?;
            out.push((a * 16 + b) as u8);
        } else {
            out.push(byte);
        }
    }
    Ok(out)
}
async fn socks(proxy: &url::Url, host: &str, port: u16) -> PluginResult<TcpStream> {
    let mut stream = TcpStream::connect((
        proxy.host_str().ok_or_else(failed)?,
        proxy.port().unwrap_or(1080),
    ))
    .await
    .map_err(|_| failed())?;
    let authenticated = !proxy.username().is_empty() || proxy.password().is_some();
    stream
        .write_all(if authenticated {
            &[5, 1, 2]
        } else {
            &[5, 1, 0]
        })
        .await
        .map_err(|_| failed())?;
    let mut response = [0; 2];
    stream
        .read_exact(&mut response)
        .await
        .map_err(|_| failed())?;
    if response != [5, if authenticated { 2 } else { 0 }] {
        return Err(failed());
    }
    if authenticated {
        let user = decode(proxy.username())?;
        let password = decode(proxy.password().unwrap_or_default())?;
        if user.len() > 255 || password.len() > 255 {
            return Err(failed());
        }
        let mut auth = vec![1, user.len() as u8];
        auth.extend(user);
        auth.push(password.len() as u8);
        auth.extend(password);
        stream.write_all(&auth).await.map_err(|_| failed())?;
        stream
            .read_exact(&mut response)
            .await
            .map_err(|_| failed())?;
        if response != [1, 0] {
            return Err(failed());
        }
    }
    let mut request = vec![5, 1, 0];
    let ip = host.parse::<std::net::IpAddr>().ok();
    if let Some(ip) = ip {
        match ip {
            std::net::IpAddr::V4(ip) => {
                request.push(1);
                request.extend(ip.octets());
            }
            std::net::IpAddr::V6(ip) => {
                request.push(4);
                request.extend(ip.octets());
            }
        }
    } else if proxy.scheme() == "socks5h" {
        if host.len() > 255 {
            return Err(failed());
        }
        request.extend([3, host.len() as u8]);
        request.extend(host.as_bytes());
    } else {
        let address = tokio::net::lookup_host((host, port))
            .await
            .map_err(|_| failed())?
            .next()
            .ok_or_else(failed)?;
        match address.ip() {
            std::net::IpAddr::V4(ip) => {
                request.push(1);
                request.extend(ip.octets());
            }
            std::net::IpAddr::V6(ip) => {
                request.push(4);
                request.extend(ip.octets());
            }
        }
    }
    request.extend(port.to_be_bytes());
    stream.write_all(&request).await.map_err(|_| failed())?;
    let mut head = [0; 4];
    stream.read_exact(&mut head).await.map_err(|_| failed())?;
    if head[..3] != [5, 0, 0] {
        return Err(failed());
    }
    let count = match head[3] {
        1 => 4,
        4 => 16,
        3 => stream.read_u8().await.map_err(|_| failed())? as usize,
        _ => return Err(failed()),
    };
    let mut tail = vec![0; count + 2];
    stream.read_exact(&mut tail).await.map_err(|_| failed())?;
    Ok(stream)
}
async fn relay(mut client: TcpStream, proxy: url::Url) -> PluginResult<()> {
    let initial = tokio::time::timeout(std::time::Duration::from_secs(20), async {
        let mut bytes = Vec::new();
        loop {
            let byte = client.read_u8().await.map_err(|_| failed())?;
            bytes.push(byte);
            if bytes.len() > 64 * 1024 {
                return Err(failed());
            }
            if bytes.ends_with(b"\r\n\r\n") {
                break;
            }
        }
        let header = std::str::from_utf8(&bytes).map_err(|_| failed())?;
        let mut request = header.lines().next().unwrap_or_default().split_whitespace();
        let method = request.next().ok_or_else(failed)?;
        let target = request.next().ok_or_else(failed)?;
        let connect = method == "CONNECT";
        let target = if connect {
            format!("http://{target}")
        } else {
            target.into()
        };
        let url = url::Url::parse(&target).map_err(|_| failed())?;
        let host = url.host_str().ok_or_else(failed)?;
        let port = url.port_or_known_default().ok_or_else(failed)?;
        if !url.username().is_empty() || url.password().is_some() {
            return Err(failed());
        }
        let mut upstream = socks(&proxy, host, port).await?;
        if connect {
            client
                .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
                .await
                .map_err(|_| failed())?;
        } else {
            if url.scheme() != "http" {
                return Err(failed());
            }
            let mut path = url.path().to_owned();
            if let Some(query) = url.query() {
                path.push('?');
                path.push_str(query);
            }
            let mut forwarded = format!("{method} {path} HTTP/1.1\r\n");
            for line in header.lines().skip(1).filter(|line| !line.is_empty()) {
                let key = line.split(':').next().unwrap_or_default();
                if !["proxy-authorization", "proxy-connection"]
                    .contains(&key.to_ascii_lowercase().as_str())
                {
                    forwarded.push_str(line);
                    forwarded.push_str("\r\n");
                }
            }
            forwarded.push_str("\r\n");
            upstream
                .write_all(forwarded.as_bytes())
                .await
                .map_err(|_| failed())?;
        }
        Ok(upstream)
    })
    .await
    .map_err(|_| failed())?;
    let mut upstream = match initial {
        Ok(upstream) => upstream,
        Err(error) => {
            let _ = client
                .write_all(
                    b"HTTP/1.1 502 Bad Gateway\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                )
                .await;
            return Err(error);
        }
    };
    tokio::io::copy_bidirectional(&mut client, &mut upstream)
        .await
        .map_err(|_| failed())?;
    Ok(())
}
#[cfg(test)]
#[path = "../test/proxy.test.rs"]
mod tests;
