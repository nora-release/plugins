use super::*;
use futures::StreamExt;
use std::sync::{Arc, Mutex};

struct Client {
    requests: Mutex<Vec<HttpRequest>>,
    replies: Mutex<Vec<Vec<u8>>>,
}
#[async_trait::async_trait]
impl HttpClient for Client {
    async fn stream(
        &self,
        request: HttpRequest,
    ) -> Result<nora_ai::HttpStreamingResponse, nora_ai::HttpClientError> {
        self.requests.lock().unwrap().push(request);
        let body = self.replies.lock().unwrap().remove(0);
        Ok(nora_ai::HttpStreamingResponse {
            status: 200,
            headers: BTreeMap::new(),
            body: Box::pin(futures::stream::once(async move { Ok(body.into()) })),
        })
    }
}

fn definition() -> Definition {
    let mut definition =
        Definition::parse(serde_json::from_str(include_str!("../nora-plugin.json")).unwrap())
            .unwrap();
    definition.adapter = Arc::new(super::super::Xai::default());
    definition
}
fn request(url: &str) -> FetchRequest {
    FetchRequest {
        caller_user_agent: None,
        constraints: Default::default(),
        subscription_accounts_enabled: true,
        account: "account".into(),
        credentials: Some(json!({"type":"api","key":"private-video-key"})),
        network_proxy: None,
        url: url.into(),
        method: "GET".into(),
        headers: BTreeMap::new(),
        metadata: BTreeMap::new(),
        body: None,
        body_file: None,
    }
}
async fn body(mut response: FetchResponse) -> Vec<u8> {
    let mut out = Vec::new();
    while let Some(chunk) = response.body.next().await {
        out.extend(chunk.unwrap());
    }
    out
}

#[tokio::test]
async fn create_returns_a_job_id_and_download_keeps_account_authentication_off_the_cdn() {
    let client=Client{requests:Mutex::new(Vec::new()),replies:Mutex::new(vec![br#"{"request_id":"upstream-job"}"#.to_vec(),br#"{"status":"done","video":{"url":"https://imagine.x.ai/video.mp4?signature=public-download"}}"#.to_vec(),b"video-bytes".to_vec()])};
    let mut create = request("https://api.x.ai/v1/videos");
    create.method = "POST".into();
    create.body = Some("eyJtb2RlbCI6Imdyb2staW1hZ2luZS12aWRlbyIsInByb21wdCI6ImNhdCJ9".into());
    let response = fetch(&definition(), &create, &client)
        .await
        .unwrap()
        .unwrap();
    let value: Value = serde_json::from_slice(&body(response).await).unwrap();
    assert_eq!(value["id"], "upstream-job");
    assert_eq!(value["status"], "queued");
    assert_eq!(value["object"], "video");
    assert_eq!(value["progress"], 0);
    let response = fetch(
        &definition(),
        &request("https://api.x.ai/v1/videos/upstream-job/content"),
        &client,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(body(response).await, b"video-bytes");
    let requests = client.requests.lock().unwrap();
    assert_eq!(requests[0].url, "https://api.x.ai/v1/videos/generations");
    assert_eq!(requests[1].url, "https://api.x.ai/v1/videos/upstream-job");
    assert_eq!(
        requests[1]
            .headers
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case("authorization"))
            .map(|(_, value)| value.as_str()),
        Some("Bearer private-video-key")
    );
    assert_eq!(
        requests[2].url,
        "https://imagine.x.ai/video.mp4?signature=public-download"
    );
    assert!(requests[2].headers.is_empty());
}

#[test]
fn video_creation_uses_generation_endpoint_and_integer_duration() {
    let mut url = Url::parse("https://api.x.ai/v1/videos").unwrap();
    let mut headers = BTreeMap::new();
    let mut body=Some(serde_json::to_vec(&json!({"model":"grok-imagine-video","prompt":"cat","seconds":"6","response_format":"b64_json","aspect_ratio":"16:9"})).unwrap());
    assert!(prepare(&mut url, &mut headers, &mut body).unwrap());
    assert_eq!(url.path(), "/v1/videos/generations");
    let payload: Value = serde_json::from_slice(body.as_ref().unwrap()).unwrap();
    assert_eq!(payload["duration"], 6);
    assert!(payload.get("seconds").is_none());
    assert!(payload.get("response_format").is_none());
}

#[test]
fn images_and_chat_do_not_share_grok_chat_overrides() {
    let mut body=Some(serde_json::to_vec(&json!({"model":"grok-imagine-image","images":[{"image_url":"data:image/png;base64,AA=="}]})).unwrap());
    assert!(prepare(
        &mut Url::parse("https://api.x.ai/v1/images/edits").unwrap(),
        &mut BTreeMap::new(),
        &mut body
    )
    .unwrap());
    let value: Value = serde_json::from_slice(body.as_ref().unwrap()).unwrap();
    assert_eq!(value["images"][0]["url"], "data:image/png;base64,AA==");
    assert_eq!(value["response_format"], "b64_json");
    assert!(!prepare(
        &mut Url::parse("https://api.x.ai/v1/responses").unwrap(),
        &mut BTreeMap::new(),
        &mut None
    )
    .unwrap());
}

#[test]
fn fractional_video_duration_is_rejected() {
    let mut body = Some(br#"{"model":"grok-imagine-video","duration":1.5}"#.to_vec());
    assert!(prepare(
        &mut Url::parse("https://api.x.ai/v1/videos").unwrap(),
        &mut BTreeMap::new(),
        &mut body
    )
    .is_err());
}

#[test]
fn video_download_urls_reject_local_targets() {
    for address in [
        "http://example.com/a",
        "https://localhost/a",
        "https://127.0.0.1/a",
        "https://10.0.0.1/a",
        "https://169.254.169.254/a",
        "https://[::1]/a",
        "https://[fc00::1]/a",
    ] {
        let url = Url::parse(address).unwrap();
        assert!(url.scheme() != "https" || !public_host(&url), "{address}");
    }
    assert!(public_host(
        &Url::parse("https://imagine.x.ai/video.mp4?signature=value").unwrap()
    ));
}

#[test]
fn pixel_sizes_and_whole_float_durations_follow_the_grok_contract() {
    for (model, resolution) in [
        ("grok-imagine-video", "720p"),
        ("grok-imagine-video-1.5", "1080p"),
    ] {
        let mut body = Some(
            serde_json::to_vec(&json!({"model":model,"duration":6.0,"size":"1920x1080"})).unwrap(),
        );
        prepare(
            &mut Url::parse("https://api.x.ai/v1/videos").unwrap(),
            &mut BTreeMap::new(),
            &mut body,
        )
        .unwrap();
        let value: Value = serde_json::from_slice(body.as_ref().unwrap()).unwrap();
        assert_eq!(value["duration"], 6);
        assert_eq!(value["aspect_ratio"], "16:9");
        assert_eq!(value["resolution"], resolution);
    }
    assert_eq!(aspect_ratio("1024x1024", false).as_deref(), Some("1:1"));
    assert_eq!(aspect_ratio("1536x1024", false).as_deref(), Some("3:2"));
}

#[tokio::test]
async fn grok_subscription_edits_convert_multipart_without_losing_image_bytes() {
    let client = Client {
        requests: Mutex::new(Vec::new()),
        replies: Mutex::new(vec![br#"{"data":[{"b64_json":"AP8="}]}"#.to_vec()]),
    };
    let mut request = request("https://cli-chat-proxy.grok.com/v1/images/edits");
    request.credentials =
        Some(json!({"type":"oauth","credentialProvider":"xAI","access":"private-image-token"}));
    request.method = "POST".into();
    request.headers.insert(
        "content-type".into(),
        "multipart/form-data; boundary=edit".into(),
    );
    let bytes=b"--edit\r\nContent-Disposition: form-data; name=\"model\"\r\n\r\ngrok-imagine-image\r\n--edit\r\nContent-Disposition: form-data; name=\"image[]\"; filename=\"a.png\"\r\nContent-Type: image/png\r\n\r\n\x00\xff\r\n--edit--\r\n";
    request.body = Some(base64::engine::general_purpose::STANDARD.encode(bytes));
    assert_eq!(
        fetch(&definition(), &request, &client)
            .await
            .unwrap()
            .unwrap()
            .status,
        200
    );
    let requests = client.requests.lock().unwrap();
    let outgoing = &requests[0];
    assert_eq!(
        outgoing.url,
        "https://cli-chat-proxy.grok.com/v1/images/edits"
    );
    assert_eq!(outgoing.headers["content-type"], "application/json");
    let value: Value = serde_json::from_slice(outgoing.body.as_ref().unwrap()).unwrap();
    assert_eq!(value["images"][0]["url"], "data:image/png;base64,AP8=");
    assert_eq!(value["images"][0]["type"], "image_url");
    assert_eq!(value["response_format"], "b64_json");
}

#[test]
fn video_states_are_public_openai_states_including_terminal_expiry() {
    for (upstream, expected, progress) in [
        ("running", "in_progress", 25),
        ("done", "completed", 100),
        ("succeeded", "completed", 100),
        ("expired", "failed", 25),
        ("cancelled", "failed", 25),
        ("failed", "failed", 25),
    ] {
        let mut value = json!({"status":upstream,"progress":25,"duration":6});
        normalize_video(&mut value).unwrap();
        assert_eq!(value["status"], expected);
        assert_eq!(value["progress"], progress);
        assert_eq!(value["seconds"], "6");
        if expected == "failed" {
            assert!(value["error"]["message"]
                .as_str()
                .is_some_and(|text| !text.is_empty()));
        }
    }
    assert!(normalize_video(&mut json!([])).is_err());
}
