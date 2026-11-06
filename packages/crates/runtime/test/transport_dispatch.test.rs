use super::*;
use async_trait::async_trait;
use std::sync::{Arc, Mutex};

#[derive(Debug, Default)]
struct Adapter(Mutex<Vec<&'static str>>);

#[async_trait]
impl crate::ProviderAdapter for Adapter {
    fn finalize_fetch(
        &self,
        _: &Value,
        _: &url::Url,
        headers: &mut BTreeMap<String, String>,
        body: &mut Option<Vec<u8>>,
    ) -> PluginResult<()> {
        assert_eq!(headers["Authorization"], "Bearer saved-token");
        headers.insert("x-final".into(), "prepared".into());
        *body = Some(b"final request body".to_vec());
        self.0.lock().unwrap().push("finalize");
        Ok(())
    }

    async fn before_send(
        &self,
        _: &Definition,
        _: &Value,
        request: &HttpRequest,
        _: &dyn HttpClient,
    ) -> PluginResult<()> {
        assert_eq!(
            request.body.as_deref(),
            Some(b"final request body".as_slice())
        );
        self.0.lock().unwrap().push("before");
        Ok(())
    }

    async fn send_fetch(
        &self,
        _: &Definition,
        credentials: &Value,
        request: HttpRequest,
        _: &dyn HttpClient,
        proxy: Option<&NetworkProxy>,
    ) -> PluginResult<HttpStreamingResponse> {
        assert_eq!(credentials["access"], "saved-token");
        assert_eq!(request.headers["Authorization"], "Bearer saved-token");
        assert_eq!(request.headers["x-final"], "prepared");
        assert_eq!(
            request.body.as_deref(),
            Some(b"final request body".as_slice())
        );
        assert_eq!(
            proxy,
            Some(&NetworkProxy::Proxy {
                url: "http://127.0.0.1:8080".into()
            })
        );
        self.0.lock().unwrap().push("send");
        Ok(HttpStreamingResponse {
            status: 200,
            headers: BTreeMap::from([("x-transport".into(), "websocket".into())]),
            body: Box::pin(futures::stream::empty()),
        })
    }

    fn observe_response(&self, _: &Value, status: u16, headers: &BTreeMap<String, String>) {
        assert_eq!(status, 200);
        assert_eq!(headers["x-transport"], "websocket");
        self.0.lock().unwrap().push("observe");
    }
}

struct NoHttp;
#[async_trait]
impl HttpClient for NoHttp {
    async fn stream(
        &self,
        _: HttpRequest,
    ) -> Result<HttpStreamingResponse, nora_ai::HttpClientError> {
        panic!("The selected provider transport must send this request");
    }
}

#[tokio::test]
async fn provider_transport_receives_final_authentication_body_and_effective_proxy() {
    let adapter = Arc::new(Adapter::default());
    let mut definition = definition("openai");
    definition.adapter = adapter.clone();
    let credentials = json!({"type":"apiKey","access":"saved-token"});
    let request = build_request(
        &definition,
        request("https://api.openai.com/v1/responses", credentials.clone()),
    )
    .await
    .unwrap();
    send_with_proxy(
        &NoHttp,
        &definition,
        &credentials,
        "openai-responses",
        request,
        Some(&NetworkProxy::Proxy {
            url: "http://127.0.0.1:8080".into(),
        }),
    )
    .await
    .unwrap();
    assert_eq!(
        *adapter.0.lock().unwrap(),
        ["finalize", "before", "send", "observe"]
    );
}
