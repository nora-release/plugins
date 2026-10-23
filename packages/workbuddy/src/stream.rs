use super::*;
use futures::StreamExt;
use std::collections::VecDeque;

struct State {
    inner: nora_ai::HttpByteStream,
    pending: Vec<u8>,
    output: VecDeque<Vec<u8>>,
    names: BTreeMap<String, String>,
    ended: bool,
}

pub(super) fn renamed(
    response: HttpStreamingResponse,
    names: BTreeMap<String, String>,
) -> FetchResponse {
    let mut headers = response.headers;
    headers.remove("content-length");
    headers.remove("content-encoding");
    headers.insert("x-magpie-sign-in".into(), "kept".into());
    if headers.iter().any(|(name, value)| {
        name.eq_ignore_ascii_case("content-type") && value.to_ascii_lowercase().contains("json")
    }) {
        let status = response.status;
        let body = futures::stream::once(async move {
            let (_, _, bytes) = super::bytes(
                HttpStreamingResponse {
                    status,
                    headers: BTreeMap::new(),
                    body: response.body,
                },
                8 * 1024 * 1024,
            )
            .await?;
            let mut value: Value = serde_json::from_slice(&bytes).map_err(|_| {
                PluginError::transport("WorkBuddy returned invalid completion JSON.")
            })?;
            tools::rename(&mut value, &names);
            Ok(value.to_string().into_bytes())
        });
        return FetchResponse {
            status,
            headers,
            body: Box::pin(body),
        };
    }
    let state = State {
        inner: response.body,
        pending: Vec::new(),
        output: VecDeque::new(),
        names,
        ended: false,
    };
    let body = futures::stream::unfold(state, |mut state| async move {
        loop {
            if let Some(output) = state.output.pop_front() {
                return Some((Ok(output), state));
            }
            if state.ended {
                return None;
            }
            match state.inner.next().await {
                Some(Ok(bytes)) => state.pending.extend_from_slice(&bytes),
                Some(Err(_)) => {
                    state.ended = true;
                    return Some((
                        Err(PluginError::transport(
                            "The WorkBuddy response stream stopped.",
                        )),
                        state,
                    ));
                }
                None => state.ended = true,
            }
            if state.pending.len() > 1024 * 1024 {
                state.ended = true;
                return Some((
                    Err(PluginError::transport(
                        "A WorkBuddy response event exceeds 1 MiB.",
                    )),
                    state,
                ));
            }
            while let Some(end) = state.pending.iter().position(|byte| *byte == b'\n') {
                let line = state.pending.drain(..=end).collect::<Vec<_>>();
                state.output.push_back(line_back(&line, &state.names));
            }
            if state.ended && !state.pending.is_empty() {
                let tail = std::mem::take(&mut state.pending);
                state.output.push_back(line_back(&tail, &state.names));
            }
        }
    });
    FetchResponse {
        status: response.status,
        headers,
        body: Box::pin(body),
    }
}
fn line_back(line: &[u8], names: &BTreeMap<String, String>) -> Vec<u8> {
    if names.is_empty() {
        return line.to_vec();
    }
    let Ok(text) = std::str::from_utf8(line) else {
        return line.to_vec();
    };
    let Some(data) = text.strip_prefix("data:") else {
        return line.to_vec();
    };
    let Ok(mut value) = serde_json::from_str::<Value>(data.trim()) else {
        return line.to_vec();
    };
    tools::rename(&mut value, names);
    format!(
        "data: {value}{}",
        if text.ends_with('\n') { "\n" } else { "" }
    )
    .into_bytes()
}

#[cfg(test)]
#[path = "../test/stream.test.rs"]
mod tests;
