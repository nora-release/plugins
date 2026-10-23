use super::*;
use nora_provider_plugin_sdk::RequestBodyFile;
use std::path::PathBuf;

fn request(bytes: &[u8], content_type: &str) -> FetchRequest {
    FetchRequest {
        account: "work".into(),
        credentials: None,
        network_proxy: None,
        url: "https://example.invalid/images/edits".into(),
        method: "POST".into(),
        headers: BTreeMap::from([("Content-Type".into(), content_type.into())]),
        metadata: BTreeMap::new(),
        body: Some(STANDARD.encode(bytes)),
        body_file: None,
    }
}
struct File(PathBuf);
impl File {
    fn new(bytes: &[u8]) -> Self {
        let path = std::env::temp_dir().join(format!("nora-upload-{}.bin", uuid::Uuid::new_v4()));
        std::fs::write(&path, bytes).unwrap();
        Self(path)
    }
}
impl Drop for File {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

#[tokio::test]
async fn json_bodies_are_parsed_without_changing_fields_and_accept_verified_body_files() {
    let bytes = br#"{"model":"custom","images":["data:image/png;base64,aQ=="],"n":2}"#;
    for file_backed in [false, true] {
        let file = File::new(bytes);
        let mut request = request(bytes, "application/json; charset=utf-8");
        if file_backed {
            request.body = None;
            request.body_file = Some(RequestBodyFile::from_path(&file.0).unwrap());
        }
        let MediaBody::Json(value) = parse(&request).await.unwrap() else {
            panic!("Expected JSON")
        };
        assert_eq!(value, serde_json::from_slice::<Value>(bytes).unwrap());
    }
}

#[tokio::test]
async fn multipart_uploads_preserve_binary_images_masks_names_and_scalar_fields() {
    let bytes=b"--boundary\r\nContent-Disposition: form-data; name=\"model\"\r\n\r\ncustom\r\n--boundary\r\nContent-Disposition: form-data; name=\"n\"\r\n\r\n2\r\n--boundary\r\nContent-Disposition: form-data; name=\"image[]\"; filename=\"bird.png\"\r\nContent-Type: image/png\r\n\r\n\x00\xffimage\r\n--boundary\r\nContent-Disposition: form-data; name=\"mask\"; filename=\"mask.png\"\r\nContent-Type: image/png\r\n\r\nmask\r\n--boundary--\r\n";
    for file_backed in [false, true] {
        let file = File::new(bytes);
        let mut request = request(bytes, "multipart/form-data; boundary=boundary");
        if file_backed {
            request.body = None;
            request.body_file = Some(RequestBodyFile::from_path(&file.0).unwrap());
        }
        let MediaBody::Multipart(form) = parse(&request).await.unwrap() else {
            panic!("Expected multipart")
        };
        assert_eq!(form.fields["model"], "custom");
        assert_eq!(form.fields["n"], "2");
        assert_eq!(form.files.len(), 2);
        assert_eq!(form.files[0].field, "image[]");
        assert_eq!(form.files[0].filename.as_deref(), Some("bird.png"));
        assert_eq!(form.files[0].bytes.as_ref(), b"\x00\xffimage");
        assert_eq!(form.files[1].field, "mask");
        assert_eq!(form.files[1].data_url(), "data:image/png;base64,bWFzaw==");
    }
}

#[tokio::test]
async fn multipart_streams_wait_for_initial_data_and_split_boundaries_before_parsing() {
    struct PendingChunks(std::collections::VecDeque<Option<Bytes>>);
    impl Stream for PendingChunks {
        type Item = Result<Bytes, std::io::Error>;

        fn poll_next(
            mut self: Pin<&mut Self>,
            context: &mut std::task::Context<'_>,
        ) -> std::task::Poll<Option<Self::Item>> {
            match self.0.pop_front() {
                Some(Some(bytes)) => std::task::Poll::Ready(Some(Ok(bytes))),
                Some(None) => {
                    context.waker().wake_by_ref();
                    std::task::Poll::Pending
                }
                None => std::task::Poll::Ready(None),
            }
        }
    }

    let body = Bytes::from_static(b"--boundary\r\nContent-Disposition: form-data; name=\"model\"\r\n\r\ncustom\r\n--boundary\r\nContent-Disposition: form-data; name=\"image\"; filename=\"bird.png\"\r\nContent-Type: image/png\r\n\r\n\x00\xffimage\r\n--boundary--\r\n");
    for chunks in [
        vec![None, Some(body.clone())],
        vec![Some(Bytes::new()), None, Some(body.clone())],
        vec![Some(body.slice(..4)), None, Some(body.slice(4..))],
        vec![Some(body.slice(..12)), None, Some(body.slice(12..))],
        vec![
            Some(body.slice(..12)),
            None,
            Some(body.slice(12..70)),
            None,
            Some(body.slice(70..)),
        ],
    ] {
        let form = parse_form(Box::pin(PendingChunks(chunks.into())), "boundary".into())
            .await
            .unwrap();
        assert_eq!(form.fields["model"], "custom");
        assert_eq!(form.files.len(), 1);
        assert_eq!(form.files[0].filename.as_deref(), Some("bird.png"));
        assert_eq!(form.files[0].bytes.as_ref(), b"\x00\xffimage");
    }
}

#[tokio::test]
async fn duplicate_fields_truncated_forms_and_file_models_are_rejected() {
    for bytes in [
        b"--boundary\r\nContent-Disposition: form-data; name=\"model\"\r\n\r\na\r\n--boundary\r\nContent-Disposition: form-data; name=\"model\"\r\n\r\nb\r\n--boundary--\r\n".as_slice(),
        b"--boundary\r\nContent-Disposition: form-data; name=\"model\"; filename=\"model.txt\"\r\n\r\na\r\n--boundary--\r\n".as_slice(),
        b"--boundary\r\nContent-Disposition: form-data; name=\"model\"\r\n\r\na".as_slice(),
    ] {assert!(parse(&request(bytes,"multipart/form-data; boundary=boundary")).await.is_err());}
    assert!(parse(&request(br#"[]"#, "application/json")).await.is_err());
    assert!(parse(&request(b"plain", "text/plain")).await.is_err());
}

#[tokio::test]
async fn a_changed_or_ambiguous_body_file_is_never_parsed() {
    let file = File::new(br#"{"model":"first"}"#);
    let descriptor = RequestBodyFile::from_path(&file.0).unwrap();
    let mut request = request(br#"{"model":"first"}"#, "application/json");
    request.body_file = Some(descriptor.clone());
    assert!(parse(&request).await.is_err());
    request.body = None;
    std::fs::write(&file.0, br#"{"model":"other"}"#).unwrap();
    let error = parse(&request).await.err().unwrap();
    assert!(!error.message.contains(file.0.to_str().unwrap()));
}

#[tokio::test]
async fn oversized_text_fields_fail_without_becoming_file_uploads() {
    let bytes=format!("--boundary\r\nContent-Disposition: form-data; name=\"prompt\"\r\n\r\n{}\r\n--boundary--\r\n","x".repeat(MAX_TEXT_FIELD_BYTES+1));
    assert!(parse(&request(
        bytes.as_bytes(),
        "multipart/form-data; boundary=boundary"
    ))
    .await
    .is_err());
}

#[test]
fn image_reference_aliases_preserve_order_and_file_ids() {
    let fields = serde_json::json!({
        "image":["https://example.test/one.png",{"image_url":{"url":"data:image/png;base64,aQ=="}}],
        "images":[{"type":"image_url","url":"https://example.test/two.png"},{"file_id":"file-image"}]
    });
    assert_eq!(
        images_from_fields(fields.as_object().unwrap()).unwrap(),
        vec![
            ImageReference::Url("https://example.test/one.png".into()),
            ImageReference::Url("data:image/png;base64,aQ==".into()),
            ImageReference::Url("https://example.test/two.png".into()),
            ImageReference::FileId("file-image".into()),
        ]
    );
    assert!(image_sources(&Value::Null).unwrap().is_empty());
    assert!(image_sources(&serde_json::json!([])).unwrap().is_empty());
}

#[test]
fn ambiguous_malformed_and_excessively_nested_image_sources_are_rejected() {
    for value in [
        serde_json::json!({"image_url":"https://a.test","file_id":"file-a"}),
        serde_json::json!({"url":"https://a.test","image_url":"https://b.test"}),
        serde_json::json!({"file_id":3}),
        serde_json::json!({"file_id":""}),
        serde_json::json!("https://a.test/\nsecret"),
        serde_json::json!({}),
        serde_json::json!(vec!["https://a.test"; 33]),
    ] {
        assert!(image_sources(&value).is_err(), "{value}");
    }
    let mut nested = serde_json::json!("https://a.test/image.png");
    for _ in 0..MAX_SOURCE_DEPTH + 1 {
        nested = serde_json::json!({"image_url":nested});
    }
    assert!(image_sources(&nested).is_err());
    let fields = serde_json::json!({"image":vec!["https://a.test";32],"images":"https://b.test"});
    assert!(images_from_fields(fields.as_object().unwrap()).is_err());
}
