use super::*;
use crate::test_support::*;
use serde_json::json;

#[tokio::test]
async fn canonical_requests_use_the_configured_endpoint_and_keep_private_metadata_out_of_http() {
    let client = Client::new(vec![image_response()]);
    let definition = definition();
    let request = request(
        "/images/generations",
        json!({"model":"gemini-image-current","prompt":"bird","n":1}),
    );
    let response = GoogleGemini
        .fetch_override(&definition, &request, &client)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(response.status, 200);
    let requests = client.requests.lock().unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].url,"https://generativelanguage.googleapis.com/v1beta/models/gemini-image-current:generateContent");
    assert_eq!(requests[0].headers["x-goog-api-key"], "private-google-key");
    assert!(!requests[0].url.contains("private-google-key"));
    assert!(!format!("{:?}", requests[0]).contains("private-session-id"));
}

#[tokio::test]
async fn foreign_urls_and_path_traversal_never_receive_credentials_or_network_requests() {
    let definition = definition();
    let client = Client::new(vec![]);
    let mut request = request(
        "/images/generations",
        json!({"model":"../another","prompt":"bird"}),
    );
    assert!(GoogleGemini
        .fetch_override(&definition, &request, &client)
        .await
        .is_err());
    request.url = "https://foreign.example/images/generations".into();
    assert!(GoogleGemini
        .fetch_override(&definition, &request, &client)
        .await
        .is_err());
    assert!(client.requests.lock().unwrap().is_empty());
}

#[tokio::test]
async fn native_upstream_errors_keep_status_and_redact_the_google_key() {
    let definition = definition();
    let client = Client::new(vec![]);
    client.push(Client::response(
        429,
        BTreeMap::from([("retry-after".into(), "30".into())]),
        br#"{"error":{"message":"private-google-key exhausted"}}"#.to_vec(),
    ));
    let request = request(
        "/images/generations",
        json!({"model":"gemini-image-current","prompt":"bird"}),
    );
    let response = GoogleGemini
        .fetch_override(&definition, &request, &client)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(response.status, 429);
    assert_eq!(response.headers["retry-after"], "30");
    let body = response_json(response).await;
    assert!(!body.to_string().contains("private-google-key"));
}

#[test]
fn actual_embedding_methods_mark_models_as_non_chat_without_model_name_guessing() {
    let mut model = Model::new(
        "semantic-vector-v2",
        "google-generative-ai",
        "google-gemini",
    );
    GoogleGemini.enrich_model(
        &json!({"supportedGenerationMethods":["embedContent","batchEmbedContents"]}),
        &mut model,
        &json!({}),
    );
    assert_eq!(model.info.embedding, Some(true));
    assert_eq!(model.info.chat, Some(false));
    assert_eq!(model.info.tool_calling, Some(false));
}
