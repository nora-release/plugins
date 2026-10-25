use super::*;

struct EditClient;
#[async_trait::async_trait]
impl HttpClient for EditClient {
    async fn stream(
        &self,
        request: nora_ai::HttpRequest,
    ) -> Result<nora_ai::HttpStreamingResponse, nora_ai::HttpClientError> {
        assert_eq!(
            request.url,
            "https://chatgpt.com/backend-api/codex/images/edits"
        );
        assert_eq!(request.headers["originator"], "codex_cli_rs");
        assert_eq!(request.headers["chatgpt-account-id"], "acct");
        assert_eq!(
            request.headers["Authorization"],
            "Bearer private-edit-token"
        );
        assert_eq!(request.headers["content-type"], "application/json");
        let value: Value = serde_json::from_slice(request.body.as_ref().unwrap()).unwrap();
        assert_eq!(value["n"], 2);
        assert_eq!(
            value["images"][0]["image_url"],
            "data:image/png;base64,AP8="
        );
        assert_eq!(value["mask"]["image_url"], "data:image/png;base64,AQ==");
        assert!(value.get("response_format").is_none());
        Ok(nora_ai::HttpStreamingResponse {
            status: 200,
            headers: BTreeMap::new(),
            body: Box::pin(futures::stream::once(async {
                Ok(br#"{"data":[{"b64_json":"AP8="}]}"#.as_slice().into())
            })),
        })
    }
}

#[tokio::test]
async fn subscription_edits_convert_real_multipart_files_to_the_codex_json_contract() {
    let mut definition =
        Definition::parse(serde_json::from_str(include_str!("../nora-provider.json")).unwrap())
            .unwrap();
    definition.adapter = std::sync::Arc::new(super::super::OpenAI);
    let bytes=b"--edit\r\nContent-Disposition: form-data; name=\"model\"\r\n\r\ngpt-image-2\r\n--edit\r\nContent-Disposition: form-data; name=\"n\"\r\n\r\n2\r\n--edit\r\nContent-Disposition: form-data; name=\"response_format\"\r\n\r\nb64_json\r\n--edit\r\nContent-Disposition: form-data; name=\"image[]\"; filename=\"a.png\"\r\nContent-Type: image/png\r\n\r\n\x00\xff\r\n--edit\r\nContent-Disposition: form-data; name=\"mask\"; filename=\"mask.png\"\r\nContent-Type: image/png\r\n\r\n\x01\r\n--edit--\r\n";
    let request = FetchRequest {
        subscription_accounts_enabled: true,
        account: "test".into(),
        credentials: Some(
            json!({"type":"oauth","credentialProvider":"openAICodex","access":"private-edit-token","extras":{"accountId":"acct"}}),
        ),
        network_proxy: None,
        url: "https://chatgpt.com/images/edits".into(),
        method: "POST".into(),
        headers: BTreeMap::from([(
            "content-type".into(),
            "multipart/form-data; boundary=edit".into(),
        )]),
        metadata: BTreeMap::new(),
        body: Some(base64::engine::general_purpose::STANDARD.encode(bytes)),
        body_file: None,
    };
    assert_eq!(
        fetch(&definition, &request, &EditClient)
            .await
            .unwrap()
            .unwrap()
            .status,
        200
    );
}

#[test]
fn codex_images_use_private_endpoint_and_supported_fields() {
    let mut url = Url::parse("https://chatgpt.com/images/edits").unwrap();
    let mut headers = BTreeMap::new();
    let mut body=Some(serde_json::to_vec(&json!({"model":"gpt-image-2","prompt":"a cat","aspect_ratio":"16:9","response_format":"b64_json","images":[{"url":"data:image/png;base64,AA=="}]})).unwrap());
    prepare(true, &mut url, &mut headers, &mut body).unwrap();
    assert_eq!(url.path(), "/backend-api/codex/images/edits");
    let value: Value = serde_json::from_slice(body.as_ref().unwrap()).unwrap();
    assert_eq!(value["size"], "1536x1024");
    assert!(value.get("response_format").is_none());
    assert_eq!(
        value["images"][0]["image_url"],
        "data:image/png;base64,AA=="
    );
    assert_eq!(headers["originator"], "codex_cli_rs");
    for name in ["x-codex-imagegen-request-id", "x-codex-image-turn-id"] {
        assert!(uuid::Uuid::parse_str(&headers[name]).is_ok());
    }
}

#[test]
fn api_media_versions_only_the_official_endpoint() {
    for (base, expected) in [
        (
            "https://api.openai.com/images/generations",
            "/v1/images/generations",
        ),
        (
            "https://relay.example/custom/images/generations",
            "/custom/images/generations",
        ),
    ] {
        let mut url = Url::parse(base).unwrap();
        let mut body = Some(
            serde_json::to_vec(&json!({"model":"dall-e-3","response_format":"b64_json"})).unwrap(),
        );
        prepare(false, &mut url, &mut BTreeMap::new(), &mut body).unwrap();
        assert_eq!(url.path(), expected);
        assert_eq!(
            serde_json::from_slice::<Value>(body.as_ref().unwrap()).unwrap()["response_format"],
            "b64_json"
        );
    }
}

#[test]
fn subscription_rejects_api_only_operations() {
    for path in ["embeddings", "videos", "audio/transcriptions"] {
        assert!(prepare(
            true,
            &mut Url::parse(&format!("https://chatgpt.com/{path}")).unwrap(),
            &mut BTreeMap::new(),
            &mut None
        )
        .is_err());
    }
}
