use futures::{stream::BoxStream, StreamExt, TryStreamExt};
use nora_provider_plugin_sdk::{PluginError, PluginResult};
use serde_json::Value;
const MAX_FRAME: usize = 32 * 1024 * 1024;
#[derive(Clone, Debug)]
pub(crate) struct Frame {
    pub name: String,
    pub text: String,
    pub lines: Vec<String>,
}
impl Frame {
    pub(crate) fn value(&self) -> PluginResult<Value> {
        let value: Value = serde_json::from_str(&self.text)
            .map_err(|_| PluginError::transport("Zen returned invalid JSON in an SSE event."))?;
        if !value.is_object() {
            return Err(PluginError::transport(
                "Zen returned a non-object SSE event.",
            ));
        }
        Ok(value)
    }
    pub(crate) fn encoded(&self) -> Vec<u8> {
        format!("{}\n\n", self.lines.join("\n")).into_bytes()
    }
    pub(crate) fn json(value: Value, name: &str) -> Self {
        let text = serde_json::to_string(&value).unwrap();
        let mut lines = Vec::new();
        if !name.is_empty() {
            lines.push(format!("event: {name}"));
        }
        lines.push(format!("data: {text}"));
        Self {
            name: name.to_owned(),
            text,
            lines,
        }
    }
    pub(crate) fn rewrite(mut self, value: Value) -> Self {
        self.text = serde_json::to_string(&value).unwrap();
        self.lines
            .retain(|line| line.split(':').next() != Some("data"));
        self.lines.push(format!("data: {}", self.text));
        self
    }
}
struct State {
    body: nora_ai::HttpByteStream,
    buffer: Vec<u8>,
    scanned: usize,
    lines: Vec<String>,
    bytes: usize,
    ended: bool,
    failed: bool,
    first: bool,
}
impl State {
    fn fail(mut self, message: &str) -> (PluginResult<Frame>, Self) {
        self.failed = true;
        self.body = Box::pin(futures::stream::empty());
        self.buffer.clear();
        self.lines.clear();
        (Err(PluginError::transport(message)), self)
    }
}
pub(crate) fn frames(body: nora_ai::HttpByteStream) -> BoxStream<'static, PluginResult<Frame>> {
    // HTTP chunks may contain many SSE events. Slice without copying so the
    // per-event bound does not depend on the transport's chunk size.
    let body = body
        .map_ok(|chunk| {
            futures::stream::unfold(chunk, |mut chunk| async move {
                if chunk.is_empty() {
                    return None;
                }
                let part = chunk.split_to(chunk.len().min(64 * 1024));
                Some((Ok(part), chunk))
            })
        })
        .try_flatten()
        .boxed();
    let state = State {
        body,
        buffer: Vec::new(),
        scanned: 0,
        lines: Vec::new(),
        bytes: 0,
        ended: false,
        failed: false,
        first: true,
    };
    Box::pin(futures::stream::unfold(state, |mut state| async move {
        if state.failed {
            return None;
        }
        loop {
            let split = state.buffer[state.scanned..]
                .iter()
                .position(|byte| *byte == b'\r' || *byte == b'\n')
                .map(|index| state.scanned + index);
            if let Some(end) = split {
                state.scanned = end;
                let last_cr = state.buffer[end] == b'\r' && end + 1 == state.buffer.len();
                if !last_cr || state.ended {
                    let count = if state.buffer[end] == b'\r'
                        && state.buffer.get(end + 1) == Some(&b'\n')
                    {
                        2
                    } else {
                        1
                    };
                    let bytes: Vec<u8> = state.buffer.drain(..end + count).collect();
                    state.scanned = 0;
                    state.bytes = state.bytes.saturating_add(bytes.len());
                    if state.bytes > MAX_FRAME {
                        return Some(state.fail("Zen's SSE event exceeds 32 MiB."));
                    }
                    let line = match std::str::from_utf8(&bytes[..end]) {
                        Ok(line) => line,
                        Err(_) => {
                            return Some(state.fail("Zen returned invalid UTF-8 in an SSE event."));
                        }
                    };
                    let line = if state.first {
                        state.first = false;
                        line.trim_start_matches('\u{feff}')
                    } else {
                        line
                    };
                    if line.is_empty() {
                        let lines = std::mem::take(&mut state.lines);
                        let mut name = String::new();
                        let mut data = Vec::new();
                        for line in &lines {
                            let (key, value) = line.split_once(':').unwrap_or((line, ""));
                            let value = value.strip_prefix(' ').unwrap_or(value);
                            if key == "event" {
                                name = value.to_owned();
                            }
                            if key == "data" {
                                data.push(value.to_owned());
                            }
                        }
                        state.bytes = 0;
                        return Some((
                            Ok(Frame {
                                name,
                                text: data.join("\n"),
                                lines,
                            }),
                            state,
                        ));
                    }
                    state.lines.push(line.to_owned());
                    continue;
                }
            } else {
                state.scanned = state.buffer.len();
            }
            if state.buffer.len().saturating_add(state.bytes) > MAX_FRAME {
                return Some(state.fail("Zen's SSE event exceeds 32 MiB."));
            }
            if state.ended {
                if state.buffer.is_empty() && state.lines.is_empty() {
                    return None;
                }
                return Some(state.fail("Zen's SSE ended inside an event."));
            }
            match tokio::time::timeout(std::time::Duration::from_secs(180), state.body.next()).await
            {
                Ok(Some(Ok(chunk))) => {
                    state.buffer.extend_from_slice(&chunk);
                }
                Ok(None) => state.ended = true,
                _ => {
                    return Some(state.fail("Zen's response stopped."));
                }
            }
        }
    }))
}
#[cfg(test)]
#[path = "../test/sse.test.rs"]
mod tests;
