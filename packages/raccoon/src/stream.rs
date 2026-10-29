use futures::StreamExt;
use nora_provider_plugin_sdk::{PluginError, ResponseBody};
use serde_json::Value;
const MAX_LINE: usize = 32 * 1024 * 1024;
pub(crate) fn rewrite_line(line: &[u8], requested: &str) -> Vec<u8> {
    let ending = if line.ends_with(b"\r\n") {
        b"\r\n".as_slice()
    } else if line.ends_with(b"\n") {
        b"\n".as_slice()
    } else {
        b"".as_slice()
    };
    let end = line.len() - ending.len();
    let content = &line[..end];
    let Some(data) = content.strip_prefix(b"data:") else {
        return line.to_vec();
    };
    let Ok(mut value) = serde_json::from_slice::<Value>(data) else {
        return line.to_vec();
    };
    if !value.is_object() || value.get("model").is_none() {
        return line.to_vec();
    }
    value["model"] = Value::String(requested.to_owned());
    let mut output = b"data: ".to_vec();
    output.extend(serde_json::to_vec(&value).unwrap());
    output.extend_from_slice(ending);
    output
}
pub(crate) fn rewrite(body: nora_ai::HttpByteStream, requested: String) -> ResponseBody {
    let state = (body, Vec::<u8>::new(), false, false);
    Box::pin(futures::stream::unfold(
        state,
        move |(mut stream, mut buffer, mut ended, failed)| {
            let requested = requested.clone();
            async move {
                if failed {
                    return None;
                }
                loop {
                    if let Some(end) = buffer.iter().position(|byte| *byte == b'\n') {
                        let line: Vec<u8> = buffer.drain(..=end).collect();
                        return Some((
                            Ok(rewrite_line(&line, &requested)),
                            (stream, buffer, ended, false),
                        ));
                    }
                    if ended {
                        if buffer.is_empty() {
                            return None;
                        }
                        let line = std::mem::take(&mut buffer);
                        return Some((
                            Ok(rewrite_line(&line, &requested)),
                            (stream, buffer, true, false),
                        ));
                    }
                    match tokio::time::timeout(std::time::Duration::from_secs(180), stream.next())
                        .await
                    {
                        Ok(Some(Ok(chunk))) => {
                            if chunk.len() > MAX_LINE.saturating_sub(buffer.len()) {
                                return Some((
                                    Err(PluginError::transport(
                                        "Raccoon's stream line exceeds 32 MiB.",
                                    )),
                                    (stream, Vec::new(), true, true),
                                ));
                            }
                            buffer.extend_from_slice(&chunk);
                        }
                        Ok(None) => ended = true,
                        _ => {
                            return Some((
                                Err(PluginError::transport("Raccoon's stream stopped.")),
                                (stream, Vec::new(), true, true),
                            ))
                        }
                    }
                }
            }
        },
    ))
}
#[cfg(test)]
#[path = "../test/stream.test.rs"]
mod tests;
