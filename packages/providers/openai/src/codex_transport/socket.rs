use futures::{SinkExt, StreamExt};
use nora_ai::HttpRequest;
use nora_provider_plugin_sdk::{NetworkProxy, PluginError};
use std::{collections::BTreeMap, time::Duration};
use tokio_tungstenite::{
    tungstenite::{
        handshake::{client::generate_key, derive_accept_key},
        protocol::{frame::coding::CloseCode, Role, WebSocketConfig},
        Message,
    },
    WebSocketStream,
};

pub(super) type Socket = WebSocketStream<reqwest::Upgraded>;

#[cfg(test)]
tokio::task_local! {
    pub(super) static EXTRA_ROOTS: Vec<tokio_rustls::rustls::pki_types::CertificateDer<'static>>;
}

pub(super) enum ConnectError {
    Permission(PluginError),
    Handshake(Option<u16>),
}

pub(super) async fn connect(
    request: &HttpRequest,
    proxy: Option<&NetworkProxy>,
    timeout: Duration,
) -> Result<(Socket, BTreeMap<String, String>), ConnectError> {
    #[cfg(test)]
    let tls = match EXTRA_ROOTS.try_with(Clone::clone) {
        Ok(roots) => nora_network::tls::client_config_with_extra_roots(roots),
        Err(_) => nora_network::tls::client_config(),
    };
    #[cfg(not(test))]
    let tls = nora_network::tls::client_config();
    let tls = tls.map_err(|_| {
        ConnectError::Permission(PluginError::transport(
            "The Codex WebSocket TLS client could not be initialized.",
        ))
    })?;
    let client = nora_provider_plugin_sdk::http_client_builder(proxy)
        .map_err(ConnectError::Permission)?
        // A preconfigured SDK TLS client may advertise h2 independently of
        // http1_only(); this upgrade must keep HTTP/1.1 at the TLS layer too.
        .use_preconfigured_tls(tls)
        .http1_only()
        .build()
        .map_err(|_| ConnectError::Handshake(None))?;
    let key = generate_key();
    let mut upgrade = client
        .get(&request.url)
        .header("connection", "Upgrade")
        .header("upgrade", "websocket")
        .header("sec-websocket-version", "13")
        .header("sec-websocket-key", &key);
    for (name, value) in super::protocol::handshake_headers(&request.headers) {
        upgrade = upgrade.header(name, value);
    }
    // The same per-attempt admission used by HTTP applies to the upgrade. A
    // refused admission must not become another attempt through the fallback.
    nora_provider_plugin_sdk::before_http_request()
        .await
        .map_err(ConnectError::Permission)?;
    tokio::time::timeout(timeout, async {
        let response = upgrade
            .send()
            .await
            .map_err(|_| ConnectError::Handshake(None))?;
        let status = response.status().as_u16();
        if status != 101 {
            return Err(ConnectError::Handshake(Some(status)));
        }
        let headers = response.headers();
        let valid = headers
            .get("upgrade")
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| value.eq_ignore_ascii_case("websocket"))
            && headers
                .get_all("connection")
                .iter()
                .filter_map(|value| value.to_str().ok())
                .flat_map(|value| value.split(','))
                .any(|value| value.trim().eq_ignore_ascii_case("upgrade"))
            && headers
                .get("sec-websocket-accept")
                .and_then(|value| value.to_str().ok())
                == Some(derive_accept_key(key.as_bytes()).as_str())
            && !headers.contains_key("sec-websocket-extensions")
            && !headers.contains_key("sec-websocket-protocol");
        if !valid {
            return Err(ConnectError::Handshake(Some(status)));
        }
        let mut headers = headers
            .iter()
            .filter_map(|(name, value)| {
                value
                    .to_str()
                    .ok()
                    .map(|value| (name.as_str().to_owned(), value.to_owned()))
            })
            .collect();
        super::protocol::response_headers(&mut headers);
        let upgraded = response
            .upgrade()
            .await
            .map_err(|_| ConnectError::Handshake(Some(status)))?;
        let config = WebSocketConfig {
            max_message_size: None,
            max_frame_size: None,
            ..Default::default()
        };
        let socket = WebSocketStream::from_raw_socket(upgraded, Role::Client, Some(config)).await;
        Ok((socket, headers))
    })
    .await
    .unwrap_or(Err(ConnectError::Handshake(None)))
}

pub(super) struct FrameError {
    pub too_big: bool,
}

pub(super) async fn frame(socket: &mut Socket) -> Result<Option<Vec<u8>>, FrameError> {
    loop {
        let Some(message) = socket.next().await else {
            return Err(FrameError { too_big: false });
        };
        match message.map_err(|_| FrameError { too_big: false })? {
            Message::Text(text) => {
                let bytes = text.trim().as_bytes().to_vec();
                return if super::protocol::kind(&bytes) == "responsesapi.websocket_timing" {
                    Ok(None)
                } else {
                    Ok(Some(bytes))
                };
            }
            Message::Close(frame) => {
                return Err(FrameError {
                    too_big: frame.is_some_and(|frame| frame.code == CloseCode::Size),
                });
            }
            Message::Ping(_) => {
                socket
                    .flush()
                    .await
                    .map_err(|_| FrameError { too_big: false })?;
            }
            Message::Pong(_) | Message::Frame(_) => {}
            Message::Binary(_) => return Ok(None),
        }
    }
}
