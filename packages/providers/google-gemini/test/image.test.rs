use super::*;
use crate::{provider::GoogleGemini, test_support::*};
use nora_provider::ProviderAdapter;

#[tokio::test]
async fn image_generations_keep_real_images_and_usage_and_skip_thought_images() {
    let client = Client::new(vec![image_response(), image_response()]);
    let request = request(
        "/images/generations",
        json!({"model":"gemini-image-current","prompt":"bird","n":2,"size":"1792x1024","response_format":"b64_json"}),
    );
    let response = GoogleGemini
        .fetch_override(&definition(), &request, &client)
        .await
        .unwrap()
        .unwrap();
    let value = response_json(response).await;
    assert_eq!(value["data"].as_array().unwrap().len(), 2);
    assert_eq!(value["data"][0]["b64_json"], "cGljdHVyZQ==");
    assert_eq!(value["data"][0]["revised_prompt"], "A painted bird");
    assert!(!value.to_string().contains("private thought"));
    assert_eq!(value["usage"]["prompt_tokens"], 8);
    assert_eq!(value["usage"]["total_tokens"], 24);
    let requests = client.requests.lock().unwrap();
    let body: Value = serde_json::from_slice(requests[0].body.as_ref().unwrap()).unwrap();
    assert_eq!(
        body["generationConfig"]["imageConfig"]["aspectRatio"],
        "16:9"
    );
    assert_eq!(
        body["generationConfig"]["responseModalities"],
        json!(["TEXT", "IMAGE"])
    );
}

#[tokio::test]
async fn json_edits_accept_data_urls_and_download_external_references_without_google_credentials() {
    let client = Client::new(vec![]);
    client.push(Client::response(
        200,
        BTreeMap::from([("content-type".into(), "application/octet-stream".into())]),
        b"\x89PNG\r\n\x1a\nimage".to_vec(),
    ));
    client.push(Client::response(
        200,
        BTreeMap::new(),
        serde_json::to_vec(&image_response()).unwrap(),
    ));
    let request = request(
        "/images/edits",
        json!({"model":"gemini-image-current","prompt":"blue bird","images":[{"image_url":{"url":"https://images.example/reference.png"}},"data:image/jpeg;base64,aW1hZ2U="]}),
    );
    let response = GoogleGemini
        .fetch_override(&definition(), &request, &client)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(response.status, 200);
    let requests = client.requests.lock().unwrap();
    assert_eq!(requests[0].url, "https://images.example/reference.png");
    assert!(requests[0].headers.is_empty());
    assert!(requests[0].body.is_none());
    let body: Value = serde_json::from_slice(requests[1].body.as_ref().unwrap()).unwrap();
    assert_eq!(
        body["contents"][0]["parts"][1]["inlineData"]["mimeType"],
        "image/png"
    );
    assert_eq!(
        body["contents"][0]["parts"][2]["inlineData"]["data"],
        "aW1hZ2U="
    );
}

#[tokio::test]
async fn malformed_references_masks_and_empty_edits_fail_before_any_upstream_request() {
    let client = Client::new(vec![]);
    for value in [
        json!({"model":"gemini-image-current","prompt":"bird"}),
        json!({"model":"gemini-image-current","prompt":"bird","mask":"data:image/png;base64,aQ==","image":"data:image/png;base64,aQ=="}),
        json!({"model":"gemini-image-current","prompt":"bird","image":"file:///private/image.png"}),
        json!({"model":"gemini-image-current","prompt":"bird","image":"data:image/png;base64,%%%%"}),
        json!({"model":"gemini-image-current","prompt":"bird","images":[{"file_id":"file-image"}]}),
        json!({"model":"gemini-image-current","prompt":"bird","image":{"url":"https://example.test/image.png","file_id":"file-image"}}),
        json!({"model":"gemini-image-current","prompt":"bird","n":5,"image":"data:image/png;base64,aQ=="}),
    ] {
        assert!(GoogleGemini
            .fetch_override(&definition(), &request("/images/edits", value), &client)
            .await
            .is_err());
    }
    assert!(client.requests.lock().unwrap().is_empty());
}

#[tokio::test]
async fn retired_imagen_models_are_not_sent_to_a_dead_google_api() {
    let client = Client::new(vec![]);
    let response = GoogleGemini
        .fetch_override(
            &definition(),
            &request(
                "/images/generations",
                json!({"model":"imagen-4.0-generate-001","prompt":"bird"}),
            ),
            &client,
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(response.status, 410);
    assert!(client.requests.lock().unwrap().is_empty());
}

#[tokio::test]
async fn asking_for_a_url_keeps_the_inline_image_google_actually_returns() {
    let client = Client::new(vec![image_response()]);
    let request = request(
        "/images/generations",
        json!({"model":"gemini-image-current","prompt":"bird","response_format":"url"}),
    );
    let response = GoogleGemini
        .fetch_override(&definition(), &request, &client)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(response.status, 200);
    let value = response_json(response).await;
    assert_eq!(value["data"][0]["b64_json"], "cGljdHVyZQ==");
    assert!(value["data"][0].get("url").is_none());
}

#[tokio::test]
async fn multipart_image_edits_are_translated_to_inline_parts_without_losing_binary_uploads() {
    let client = Client::new(vec![image_response()]);
    let mut request = request("/images/edits", json!({}));
    let bytes=b"--boundary\r\nContent-Disposition: form-data; name=\"model\"\r\n\r\ngemini-image-current\r\n--boundary\r\nContent-Disposition: form-data; name=\"prompt\"\r\n\r\nblue bird\r\n--boundary\r\nContent-Disposition: form-data; name=\"image[]\"; filename=\"bird.png\"\r\nContent-Type: image/png\r\n\r\n\x00\xffimage\r\n--boundary--\r\n";
    request.body = Some(STANDARD.encode(bytes));
    request.headers.insert(
        "content-type".into(),
        "multipart/form-data; boundary=boundary".into(),
    );
    GoogleGemini
        .fetch_override(&definition(), &request, &client)
        .await
        .unwrap()
        .unwrap();
    let requests = client.requests.lock().unwrap();
    let body: Value = serde_json::from_slice(requests[0].body.as_ref().unwrap()).unwrap();
    assert_eq!(
        body["contents"][0]["parts"][1]["inlineData"]["data"],
        STANDARD.encode(b"\x00\xffimage")
    );
    assert_eq!(requests[0].headers["content-type"], "application/json");
}

#[test]
fn pixel_dimensions_choose_the_nearest_supported_gemini_ratio() {
    for (size, expected) in [
        ("1792x1024", Some("16:9")),
        ("1024x1792", Some("9:16")),
        ("1024x768", Some("4:3")),
        ("999x1024", Some("1:1")),
        (" 1792X1024 ", Some("16:9")),
        ("4:5", Some("4:5")),
        ("auto", None),
        ("", None),
    ] {
        assert_eq!(
            aspect_ratio(json!({"size":size}).as_object().unwrap()).unwrap(),
            expected.map(str::to_owned),
            "{size}"
        );
    }
    for size in ["0x1024", "NaNx1024", "1xinf", "square"] {
        assert!(aspect_ratio(json!({"size":size}).as_object().unwrap()).is_err());
    }
    assert_eq!(
        aspect_ratio(
            json!({"size":"1792x1024","aspect_ratio":"3:2"})
                .as_object()
                .unwrap()
        )
        .unwrap(),
        Some("3:2".into())
    );
}

#[tokio::test]
async fn reference_budget_counts_base64_expansion_across_uploads_and_downloads() {
    let bytes = b"reference-image";
    let one_size = encoded_length(bytes.len())
        + json!({"inlineData":{"mimeType":"image/png","data":""}})
            .to_string()
            .len();
    let mut budget = ReferenceBudget::new(one_size * 2 - 1);
    inline_image("image/png", bytes, &mut budget).unwrap();
    let used = budget.used;
    assert!(inline_image("image/png", bytes, &mut budget).is_err());
    assert_eq!(budget.used, used);

    let client = Client::new(vec![]);
    let data_url = format!("data:image/png;base64,{}", STANDARD.encode(bytes));
    assert!(
        reference_image(&data_url, &client, encoded_length(bytes.len()) - 1)
            .await
            .is_err()
    );
    assert!(client.requests.lock().unwrap().is_empty());
    client.push(Client::response(
        200,
        BTreeMap::from([("content-type".into(), "image/png".into())]),
        bytes.to_vec(),
    ));
    assert!(reference_image(
        "https://images.example/reference.png",
        &client,
        encoded_length(bytes.len()) - 1
    )
    .await
    .is_err());

    let mut production_budget = ReferenceBudget::new(MAX_MEDIA_BODY_BYTES);
    for _ in 0..5 {
        production_budget
            .reserve("image/png", MAX_IMAGE_BYTES)
            .unwrap();
    }
    assert!(production_budget
        .reserve("image/png", MAX_IMAGE_BYTES)
        .is_err());
}

#[tokio::test]
async fn partial_batches_keep_paid_images_when_later_calls_fail_and_sum_reported_usage() {
    for failure in ["http", "transport", "no-image", "invalid-image"] {
        let client = Client::new(vec![image_response()]);
        if failure == "http" {
            client.push(Client::response(
                429,
                BTreeMap::from([("retry-after".into(), "17".into())]),
                br#"{"error":{"message":"Rate limited"}}"#.to_vec(),
            ));
        } else if failure != "transport" {
            client.push(Client::response(
                200,
                BTreeMap::new(),
                serde_json::to_vec(&json!({
                    "candidates":[{"content":{"parts":if failure=="invalid-image" {json!([{"inlineData":{"mimeType":"image/png","data":"%%%%"}}])} else {json!([{"text":"No image"}])}}}],
                    "usageMetadata":{"promptTokenCount":3,"candidatesTokenCount":1,"totalTokenCount":4}
                })).unwrap(),
            ));
        }
        let response = GoogleGemini
            .fetch_override(
                &definition(),
                &request(
                    "/images/generations",
                    json!({"model":"gemini-image-current","prompt":"bird","n":2}),
                ),
                &client,
            )
            .await
            .unwrap()
            .unwrap();
        assert_eq!(response.status, 200, "{failure}");
        let value = response_json(response).await;
        assert_eq!(value["data"].as_array().unwrap().len(), 1, "{failure}");
        assert_eq!(value["data"][0]["b64_json"], "cGljdHVyZQ==", "{failure}");
        assert_eq!(
            value["usage"]["total_tokens"],
            if matches!(failure, "no-image" | "invalid-image") {
                16
            } else {
                12
            },
            "{failure}"
        );
        assert_eq!(client.requests.lock().unwrap().len(), 2, "{failure}");
    }
}

#[tokio::test]
async fn failed_first_draw_does_not_hide_a_later_success() {
    let client = Client::new(vec![]);
    client.push(Client::response(
        503,
        BTreeMap::new(),
        br#"{"error":{"message":"Unavailable"}}"#.to_vec(),
    ));
    client.push(Client::response(
        200,
        BTreeMap::new(),
        serde_json::to_vec(&image_response()).unwrap(),
    ));
    let response = GoogleGemini
        .fetch_override(
            &definition(),
            &request(
                "/images/generations",
                json!({"model":"gemini-image-current","prompt":"bird","n":2}),
            ),
            &client,
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(response.status, 200);
    let value = response_json(response).await;
    assert_eq!(value["data"].as_array().unwrap().len(), 1);
    assert_eq!(value["usage"]["total_tokens"], 12);
}

#[tokio::test]
async fn batches_with_no_images_preserve_the_first_actual_http_failure() {
    let client = Client::new(vec![]);
    client.push(Client::response(
        429,
        BTreeMap::from([("retry-after".into(), "17".into())]),
        br#"{"error":{"message":"Rate limited"}}"#.to_vec(),
    ));
    let response = GoogleGemini
        .fetch_override(
            &definition(),
            &request(
                "/images/generations",
                json!({"model":"gemini-image-current","prompt":"bird","n":2}),
            ),
            &client,
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(response.status, 429);
    assert_eq!(response.headers["retry-after"], "17");
    assert_eq!(
        response_json(response).await["error"]["message"],
        "Rate limited"
    );
    assert_eq!(client.requests.lock().unwrap().len(), 2);
}
