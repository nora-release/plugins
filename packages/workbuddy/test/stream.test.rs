use super::*;

#[tokio::test]
async fn tool_renaming_handles_utf8_and_json_split_across_http_chunks() {
    let bytes="data: {\"choices\":[{\"delta\":{\"content\":\"你好\",\"tool_calls\":[{\"index\":0,\"function\":{\"name\":\"read_file\",\"arguments\":\"{}\"}}]}}]}\n\ndata: [DONE]\n\n".as_bytes().to_vec();
    let chunks = bytes
        .chunks(7)
        .map(|chunk| Ok(chunk.to_vec().into()))
        .collect::<Vec<_>>();
    let response = HttpStreamingResponse {
        status: 200,
        headers: BTreeMap::from([("content-type".into(), "text/event-stream".into())]),
        body: Box::pin(futures::stream::iter(chunks)),
    };
    let mut response = renamed(
        response,
        BTreeMap::from([("read_file".into(), "read.file".into())]),
    );
    let mut bytes = Vec::new();
    while let Some(chunk) = response.body.next().await {
        bytes.extend_from_slice(&chunk.unwrap());
    }
    let text = String::from_utf8(bytes).unwrap();
    assert!(text.contains("你好"));
    assert!(text.contains("read.file"));
    assert!(text.ends_with("data: [DONE]\n\n"));
}
