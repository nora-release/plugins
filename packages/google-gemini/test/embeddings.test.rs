use super::*;
use crate::{provider::GoogleGemini, test_support::*};
use nora_provider::ProviderAdapter;

#[tokio::test]
async fn single_embeddings_use_the_native_method_and_only_report_upstream_usage() {
    let client = Client::new(vec![
        json!({"embedding":{"values":[0.25,-0.5]},"usageMetadata":{"promptTokenCount":7}}),
    ]);
    let response = GoogleGemini
        .fetch_override(
            &definition(),
            &request(
                "/embeddings",
                json!({"model":"semantic-vector-v2","input":"bird","dimensions":2}),
            ),
            &client,
        )
        .await
        .unwrap()
        .unwrap();
    let value = response_json(response).await;
    assert_eq!(value["data"][0]["embedding"], json!([0.25, -0.5]));
    assert_eq!(value["usage"]["total_tokens"], 7);
    let requests = client.requests.lock().unwrap();
    assert!(requests[0]
        .url
        .ends_with("/models/semantic-vector-v2:embedContent"));
    let body: Value = serde_json::from_slice(requests[0].body.as_ref().unwrap()).unwrap();
    assert_eq!(body["embedContentConfig"]["outputDimensionality"], 2);
}

#[tokio::test]
async fn batch_embeddings_keep_input_order_and_encode_little_endian_float32_when_requested() {
    let client = Client::new(vec![
        json!({"embeddings":[{"values":[1.0,-1.0]},{"values":[0.0,0.5]}]}),
    ]);
    let response=GoogleGemini.fetch_override(&definition(),&request("/embeddings",json!({"model":"semantic-vector-v2","input":["one","two"],"encoding_format":"base64","task_type":"RETRIEVAL_DOCUMENT","title":"Birds"})),&client).await.unwrap().unwrap();
    let value = response_json(response).await;
    assert_eq!(
        value["data"][0]["embedding"],
        STANDARD.encode([1f32.to_le_bytes(), (-1f32).to_le_bytes()].concat())
    );
    assert_eq!(value["data"][1]["index"], 1);
    assert!(value.get("usage").is_none());
    let requests = client.requests.lock().unwrap();
    assert!(requests[0].url.ends_with(":batchEmbedContents"));
    let body: Value = serde_json::from_slice(requests[0].body.as_ref().unwrap()).unwrap();
    assert_eq!(body["requests"][1]["content"]["parts"][0]["text"], "two");
    assert_eq!(
        body["requests"][0]["embedContentConfig"]["taskType"],
        "RETRIEVAL_DOCUMENT"
    );
}

#[tokio::test]
async fn token_ids_invalid_dimensions_and_partial_vectors_never_report_success() {
    let client = Client::new(vec![]);
    for input in [
        json!({"input":[1,2]}),
        json!({"input":"bird","dimensions":0}),
        json!({"input":[]}),
        json!({"input":"bird","encoding_format":"hex"}),
    ] {
        let mut value = input;
        value["model"] = json!("semantic-vector-v2");
        assert!(GoogleGemini
            .fetch_override(&definition(), &request("/embeddings", value), &client)
            .await
            .is_err());
    }
    assert!(client.requests.lock().unwrap().is_empty());
    client.push(Client::response(
        200,
        Default::default(),
        br#"{"embeddings":[{"values":[1]}]}"#.to_vec(),
    ));
    assert!(GoogleGemini
        .fetch_override(
            &definition(),
            &request(
                "/embeddings",
                json!({"model":"semantic-vector-v2","input":["one","two"]})
            ),
            &client
        )
        .await
        .is_err());
}
