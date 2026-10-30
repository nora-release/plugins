use futures::{stream, SinkExt};
use nora_ai::{HttpClient, HttpClientError, HttpMethod, HttpRequest, HttpStreamingResponse};
use nora_provider_plugin_sdk::{NetworkProxy, PluginError, PluginResult};
use std::time::Duration;
use tokio_tungstenite::tungstenite::Message;

#[path = "codex_transport/protocol.rs"]
mod protocol;
#[path = "codex_transport/socket.rs"]
mod socket;
#[path = "codex_transport/state.rs"]
mod state;

const PRELUDE_FRAMES: usize = 8;
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(30);

/// This is deliberately below the provider's final request preparation: both
/// transports must send the same account, body and caller-specific adaptations.
pub(crate) async fn send(
    request: HttpRequest,
    client: &dyn HttpClient,
    proxy: Option<&NetworkProxy>,
) -> PluginResult<HttpStreamingResponse> {
    if !is_turn(&request) {
        return http(request, client).await;
    }
    send_turn(request, client, proxy).await
}

fn is_turn(request: &HttpRequest) -> bool {
    if request.method != HttpMethod::Post {
        return false;
    }
    url::Url::parse(&request.url).is_ok_and(|url| {
        url.scheme() == "https"
            && url.host_str() == Some("chatgpt.com")
            && url.port_or_known_default() == Some(443)
            && url.path() == "/backend-api/codex/responses"
    })
}

async fn send_turn(
    request: HttpRequest,
    client: &dyn HttpClient,
    proxy: Option<&NetworkProxy>,
) -> PluginResult<HttpStreamingResponse> {
    let Some(message) = protocol::message(&request) else {
        return http(request, client).await;
    };
    let host = url::Url::parse(&request.url)
        .map(|url| url[url::Position::BeforeHost..url::Position::AfterPort].to_owned())
        .unwrap_or_default();
    if state::blocked(&host, message.len()) {
        return http(request, client).await;
    }
    match websocket(&request, &message, &host, proxy).await? {
        Some(response) => Ok(response),
        None => http(request, client).await,
    }
}

async fn http(
    request: HttpRequest,
    client: &dyn HttpClient,
) -> PluginResult<HttpStreamingResponse> {
    client
        .stream(request)
        .await
        .map_err(|_| PluginError::transport("The provider endpoint could not be reached."))
}

async fn websocket(
    request: &HttpRequest,
    message: &str,
    host: &str,
    proxy: Option<&NetworkProxy>,
) -> PluginResult<Option<HttpStreamingResponse>> {
    let (mut socket, mut headers) = match socket::connect(request, proxy, HANDSHAKE_TIMEOUT).await {
        Ok(connection) => connection,
        Err(socket::ConnectError::Permission(error)) => return Err(error),
        Err(socket::ConnectError::Handshake(status)) => {
            if !status.is_some_and(|status| status == 401 || status == 429 || status >= 500) {
                state::refused(host);
            }
            return Ok(None);
        }
    };
    if socket
        .send(Message::Text(message.to_owned()))
        .await
        .is_err()
    {
        return Ok(None);
    }
    let mut pending = Vec::new();
    let mut done = false;
    for _ in 0..PRELUDE_FRAMES {
        let frame = match socket::frame(&mut socket).await {
            Ok(Some(frame)) => frame,
            Ok(None) => continue,
            Err(error) => {
                if error.too_big {
                    state::too_big(host, message.len());
                }
                return Ok(None);
            }
        };
        let kind = protocol::kind(&frame);
        match kind.as_str() {
            "codex.rate_limits" => protocol::limit_headers(&mut headers, &frame),
            "codex.response.metadata" => protocol::metadata_headers(&mut headers, &frame),
            "error" => match protocol::error_response(&headers, &frame) {
                protocol::ErrorResponse::Fallback => return Ok(None),
                protocol::ErrorResponse::Response(response) => return Ok(Some(response)),
                protocol::ErrorResponse::Stream => {}
            },
            _ => {}
        }
        done = protocol::terminal(&kind);
        pending.push(protocol::sse(&kind, &frame));
        if !matches!(
            kind.as_str(),
            "codex.rate_limits" | "codex.response.metadata"
        ) {
            break;
        }
    }
    protocol::set_header(&mut headers, "content-type", "text/event-stream".into());
    // Once a response is returned, losing its socket must end this body with an
    // error. Replaying it over HTTP could execute the same turn twice.
    let body = stream::unfold(
        (socket, pending.into_iter(), done),
        |(mut socket, mut pending, mut done)| async move {
            if let Some(bytes) = pending.next() {
                return Some((Ok(bytes.into()), (socket, pending, done)));
            }
            if done {
                return None;
            }
            loop {
                match socket::frame(&mut socket).await {
                    Ok(Some(frame)) => {
                        let kind = protocol::kind(&frame);
                        done = protocol::terminal(&kind);
                        let bytes = protocol::sse(&kind, &frame);
                        return Some((Ok(bytes.into()), (socket, pending, done)));
                    }
                    Ok(None) => {}
                    Err(_) => {
                        let error = HttpClientError::Transport(
                            "The Codex WebSocket stopped before the response completed.".into(),
                        );
                        return Some((Err(error), (socket, pending, true)));
                    }
                }
            }
        },
    );
    Ok(Some(HttpStreamingResponse {
        status: 200,
        headers,
        body: Box::pin(body),
    }))
}

#[cfg(test)]
#[path = "../test/codex_transport.test.rs"]
mod tests;

#[cfg(test)]
#[path = "../test/codex_transport_tls.test.rs"]
mod tls_tests;
