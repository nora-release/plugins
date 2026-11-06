use async_trait::async_trait;
use bytes::Bytes;
use futures::StreamExt;
use nora_ai::HttpMethod;
use nora_provider_plugin_sdk::{NetworkProxy, PluginError, PluginResult, ResponseBody};
use std::{collections::BTreeMap, time::Duration};
pub(crate) struct Request {
    pub method: HttpMethod,
    pub url: String,
    pub headers: BTreeMap<String, String>,
    pub body: Option<Bytes>,
}
impl Request {
    pub(crate) fn get(url: impl Into<String>, headers: BTreeMap<String, String>) -> Self {
        Self {
            method: HttpMethod::Get,
            url: url.into(),
            headers,
            body: None,
        }
    }
}
pub(crate) struct Reply {
    pub status: u16,
    pub headers: BTreeMap<String, String>,
    pub set_cookies: Vec<String>,
    pub body: ResponseBody,
}
#[async_trait]
pub(crate) trait Net: Send + Sync {
    async fn send(
        &self,
        request: Request,
        proxy: Option<&NetworkProxy>,
        timeout: Duration,
    ) -> PluginResult<Reply>;
}
pub(crate) struct Http;
#[async_trait]
impl Net for Http {
    async fn send(
        &self,
        request: Request,
        proxy: Option<&NetworkProxy>,
        timeout: Duration,
    ) -> PluginResult<Reply> {
        let client = nora_provider_plugin_sdk::http_client_builder(proxy)?
            .connect_timeout(Duration::from_secs(20))
            .build()
            .map_err(|_| {
                PluginError::transport("Xiaomi MiMo's network settings could not be applied.")
            })?;
        let method = match request.method {
            HttpMethod::Get => reqwest::Method::GET,
            HttpMethod::Post => reqwest::Method::POST,
            _ => {
                return Err(PluginError::invalid(
                    "Unsupported Xiaomi MiMo request method.",
                ))
            }
        };
        let mut builder = client.request(method, &request.url);
        for (key, value) in request.headers {
            builder = builder.header(key, value);
        }
        if let Some(body) = request.body {
            builder = builder.body(body);
        }
        let response = tokio::time::timeout(timeout, nora_provider_plugin_sdk::send_http(builder))
            .await
            .map_err(|_| PluginError::transport("Xiaomi MiMo did not answer in time."))?
            .map_err(|_| PluginError::transport("Xiaomi MiMo could not be reached."))?;
        let status = response.status().as_u16();
        let headers = response
            .headers()
            .iter()
            .filter_map(|(key, value)| {
                value
                    .to_str()
                    .ok()
                    .map(|value| (key.as_str().to_owned(), value.to_owned()))
            })
            .collect();
        let set_cookies = response
            .headers()
            .get_all(reqwest::header::SET_COOKIE)
            .iter()
            .filter_map(|value| value.to_str().ok().map(str::to_owned))
            .collect();
        let source: std::pin::Pin<
            Box<dyn futures::Stream<Item = Result<Bytes, reqwest::Error>> + Send>,
        > = Box::pin(response.bytes_stream());
        let body = futures::stream::unfold(source, |mut stream| async move {
            match tokio::time::timeout(Duration::from_secs(180), stream.next()).await {
                Ok(Some(Ok(bytes))) => Some((Ok(bytes.to_vec()), stream)),
                Ok(None) => None,
                _ => Some((
                    Err(PluginError::transport("Xiaomi MiMo's response stopped.")),
                    Box::pin(futures::stream::empty())
                        as std::pin::Pin<
                            Box<dyn futures::Stream<Item = Result<Bytes, reqwest::Error>> + Send>,
                        >,
                )),
            }
        });
        Ok(Reply {
            status,
            headers,
            set_cookies,
            body: Box::pin(body),
        })
    }
}
pub(crate) async fn collect(
    mut reply: Reply,
    max: usize,
) -> PluginResult<(u16, BTreeMap<String, String>, Vec<String>, Vec<u8>)> {
    let mut bytes = Vec::new();
    while let Some(chunk) = reply.body.next().await {
        let chunk = chunk?;
        if chunk.len() > max.saturating_sub(bytes.len()) {
            return Err(PluginError::transport(
                "Xiaomi MiMo's JSON response exceeds its size limit.",
            ));
        }
        bytes.extend(chunk);
    }
    Ok((reply.status, reply.headers, reply.set_cookies, bytes))
}
pub(crate) async fn read(
    net: &dyn Net,
    request: Request,
    proxy: Option<&NetworkProxy>,
    timeout: Duration,
) -> PluginResult<(u16, BTreeMap<String, String>, Vec<String>, Vec<u8>)> {
    tokio::time::timeout(timeout, async {
        collect(net.send(request, proxy, timeout).await?, 16 * 1024 * 1024).await
    })
    .await
    .map_err(|_| PluginError::transport("Xiaomi MiMo's account request timed out."))?
}
#[cfg(test)]
#[path = "../test/net.test.rs"]
mod tests;
