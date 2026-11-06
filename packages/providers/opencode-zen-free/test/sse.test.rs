use super::*;
use std::{
    collections::VecDeque,
    pin::Pin,
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc,
    },
    task::{Context, Poll},
};

#[derive(Default)]
struct Probe {
    polls: AtomicUsize,
    dropped: AtomicBool,
}
struct ObservedBody {
    chunks: VecDeque<<nora_ai::HttpByteStream as futures::Stream>::Item>,
    probe: Arc<Probe>,
}
impl futures::Stream for ObservedBody {
    type Item = <nora_ai::HttpByteStream as futures::Stream>::Item;
    fn poll_next(mut self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.probe.polls.fetch_add(1, Ordering::SeqCst);
        Poll::Ready(self.chunks.pop_front())
    }
}
impl Drop for ObservedBody {
    fn drop(&mut self) {
        self.probe.dropped.store(true, Ordering::SeqCst);
    }
}
fn observed(chunks: Vec<Vec<u8>>) -> (nora_ai::HttpByteStream, Arc<Probe>) {
    let probe = Arc::new(Probe::default());
    let stream = ObservedBody {
        chunks: chunks.into_iter().map(|chunk| Ok(chunk.into())).collect(),
        probe: probe.clone(),
    };
    (Box::pin(stream), probe)
}
fn body(bytes: Vec<u8>) -> nora_ai::HttpByteStream {
    let chunks: Vec<_> = bytes
        .chunks(1)
        .map(|byte| Ok(byte.to_vec().into()))
        .collect();
    Box::pin(futures::stream::iter(chunks))
}
#[tokio::test]
async fn fragmented_utf8_crlf_multiline_data_and_comments_keep_event_boundaries() {
    let input="\u{feff}: hello\r\nevent: thing\r\ndata: {\r\ndata: \"text\":\"中文\"}\r\n\r\ndata: [DONE]\r\n\r\n";
    let values = frames(body(input.as_bytes().to_vec()))
        .collect::<Vec<_>>()
        .await
        .into_iter()
        .collect::<PluginResult<Vec<_>>>()
        .unwrap();
    assert_eq!(values.len(), 2);
    assert_eq!(values[0].name, "thing");
    assert_eq!(values[0].value().unwrap()["text"], "中文");
    assert_eq!(values[1].text, "[DONE]");
}
#[tokio::test]
async fn a_truncated_frame_or_invalid_utf8_is_an_error() {
    for bytes in [
        b"data: {}\n".to_vec(),
        vec![b'd', b'a', b't', b'a', b':', b' ', 255, b'\n', b'\n'],
    ] {
        let values = frames(body(bytes)).collect::<Vec<_>>().await;
        assert!(values.iter().any(Result::is_err));
    }
}
#[test]
fn rewritten_frames_preserve_other_sse_fields() {
    let frame = Frame {
        name: "event".into(),
        text: "{}".into(),
        lines: vec![
            "id: unchanged".into(),
            "event: event".into(),
            "data: {}".into(),
        ],
    };
    let output = frame.rewrite(serde_json::json!({"text":"new"})).encoded();
    let text = String::from_utf8(output).unwrap();
    assert!(text.contains("id: unchanged"));
    assert!(text.contains("\"text\":\"new\""));
}

#[tokio::test]
async fn cr_only_events_and_a_split_bom_follow_the_same_sse_contract() {
    let input =
        "\u{feff}: comment\revent: example\rdata: {\rdata: \"text\":\"中文\"}\r\rdata: [DONE]\r\r";
    let values = frames(body(input.as_bytes().to_vec()))
        .collect::<Vec<_>>()
        .await
        .into_iter()
        .collect::<PluginResult<Vec<_>>>()
        .unwrap();
    assert_eq!(values.len(), 2);
    assert_eq!(values[0].name, "example");
    assert_eq!(values[0].value().unwrap()["text"], "中文");
    assert_eq!(values[1].text, "[DONE]");
}

#[tokio::test]
async fn an_oversized_event_closes_the_upstream_before_the_error_is_delivered() {
    let (source, probe) = observed(vec![
        vec![b'x'; MAX_FRAME + 1],
        b"must not be read".to_vec(),
    ]);
    let mut stream = frames(source);
    assert!(stream.next().await.unwrap().is_err());
    assert_eq!(probe.polls.load(Ordering::SeqCst), 1);
    assert!(probe.dropped.load(Ordering::SeqCst));
    assert!(stream.next().await.is_none());
}

#[tokio::test]
async fn each_event_has_its_own_size_budget_even_when_several_share_one_network_chunk() {
    let text = "x".repeat(MAX_FRAME / 2 + 256);
    let chunk = format!("data: {text}\n\ndata: {text}\n\n").into_bytes();
    assert!(chunk.len() > MAX_FRAME);
    let (source, _) = observed(vec![chunk]);
    let values = frames(source)
        .collect::<Vec<_>>()
        .await
        .into_iter()
        .collect::<PluginResult<Vec<_>>>()
        .unwrap();
    assert_eq!(values.len(), 2);
    assert!(values.iter().all(|frame| frame.text == text));
}

#[tokio::test]
async fn crlf_bytes_count_toward_the_event_limit_even_when_the_pair_is_split() {
    for excess in [0, 1] {
        let first = format!("data: {}\r", "x".repeat(MAX_FRAME - 10 + excess)).into_bytes();
        let (source, probe) = observed(vec![first, b"\n\r".to_vec(), b"\n".to_vec()]);
        let mut stream = frames(source);
        let result = stream.next().await.unwrap();
        if excess == 0 {
            assert!(result.is_ok());
        } else {
            assert!(result.is_err());
            assert!(probe.dropped.load(Ordering::SeqCst));
        }
    }
}

#[tokio::test]
async fn downstream_drop_releases_the_source_without_reading_the_next_chunk() {
    let (source, probe) = observed(vec![b"data: {}\n\n".to_vec(), b"data: {}\n\n".to_vec()]);
    let mut stream = frames(source);
    assert!(stream.next().await.unwrap().is_ok());
    assert_eq!(probe.polls.load(Ordering::SeqCst), 1);
    drop(stream);
    assert!(probe.dropped.load(Ordering::SeqCst));
    assert_eq!(probe.polls.load(Ordering::SeqCst), 1);
}
