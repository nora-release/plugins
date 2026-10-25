use futures::{stream, StreamExt};
use nora_ai::HttpStreamingResponse;

pub(crate) const ERROR_PREFIX: usize = 64 * 1024;

/// Inspects a bounded prefix while preserving the original chunks and errors
/// for the eventual consumer, including a chunk larger than the prefix limit.
pub(crate) async fn error_prefix(response: &mut HttpStreamingResponse) -> Vec<u8> {
    let mut bytes = Vec::new();
    let mut kept = Vec::new();
    while bytes.len() < ERROR_PREFIX {
        let Some(chunk) = response.body.next().await else {
            break;
        };
        match chunk {
            Ok(chunk) => {
                let length = chunk.len().min(ERROR_PREFIX - bytes.len());
                bytes.extend_from_slice(&chunk[..length]);
                kept.push(Ok(chunk));
            }
            Err(error) => {
                kept.push(Err(error));
                break;
            }
        }
    }
    let body = std::mem::replace(&mut response.body, Box::pin(stream::empty()));
    response.body = Box::pin(stream::iter(kept).chain(body));
    bytes
}
