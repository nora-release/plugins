use super::{
    auth,
    session::{Event, Guard},
};
use nora_provider_plugin_sdk::{FetchResponse, PluginError, PluginResult};
use serde_json::{json, Value};
use std::collections::{BTreeMap, VecDeque};
struct State {
    guard: Guard,
    id: String,
    model: String,
    created: u64,
    first: Option<Event>,
    queue: VecDeque<Vec<u8>>,
    done: bool,
}
impl State {
    fn chunk(&self, delta: Value, stop: Option<&str>, usage: Option<Value>) -> Vec<u8> {
        let mut value = json!({"id":self.id,"object":"chat.completion.chunk","created":self.created,"model":self.model,"choices":[{"index":0,"delta":delta,"finish_reason":stop}]});
        if let Some(usage) = usage {
            value["usage"] = usage;
        }
        format!("data: {value}\n\n").into_bytes()
    }
    async fn next(&mut self) -> PluginResult<Option<Vec<u8>>> {
        if let Some(bytes) = self.queue.pop_front() {
            if self.done {
                self.guard.run.delivered();
                self.guard.complete = true;
            }
            return Ok(Some(bytes));
        }
        if self.done {
            self.guard.run.delivered();
            self.guard.complete = true;
            return Ok(None);
        }
        let event = match self.first.take() {
            Some(e) => Some(e),
            None => self.guard.run.next().await?,
        };
        match event{
            Some(Event::Text(text))=>Ok(Some(self.chunk(json!({"content":text}),None,None))),
            Some(Event::Reasoning(text))=>Ok(Some(self.chunk(json!({"reasoning_content":text}),None,None))),
            Some(Event::Tool(index,c))=>Ok(Some(self.chunk(json!({"tool_calls":[{"index":index,"id":c.id,"type":"function","function":{"name":c.name,"arguments":c.args.to_string()}}]}),None,None))),
            Some(Event::Finish(stop,usage))=>{let last=self.chunk(json!({}),Some(stop),Some(usage));self.guard.run.usage_delivered();self.queue.push_back(b"data: [DONE]\n\n".to_vec());self.done=true;Ok(Some(last))},
            None=>Err(PluginError::transport("Cursor did not finish its reply."))
        }
    }
}
pub(crate) fn failure(error: PluginError) -> FetchResponse {
    let status = if error.code == "authentication_required" {
        401
    } else if error.code == "unsupported_media_type" {
        415
    } else if error.code == "invalid_request" {
        400
    } else if error.code == "cursor_region_required" {
        403
    } else {
        error
            .code
            .strip_prefix("cursor_http_")
            .and_then(|s| s.parse::<u16>().ok())
            .filter(|s| (400..600).contains(s))
            .unwrap_or(502)
    };
    FetchResponse::bytes(
        status,
        BTreeMap::from([("content-type".into(), "application/json".into())]),
        serde_json::to_vec(
            &json!({"error":{"type":"cursor_error","code":status,"message":error.message}}),
        )
        .unwrap(),
    )
}
pub(crate) async fn response(mut guard: Guard, chat: &Value) -> PluginResult<FetchResponse> {
    let first = guard
        .run
        .next()
        .await?
        .ok_or_else(|| PluginError::transport("Cursor returned no reply."))?;
    let id = format!("chatcmpl-{}", auth::random());
    let model = chat["model"].as_str().unwrap_or_default().to_owned();
    let created = auth::now() / 1000;
    let mut state = State {
        guard,
        id,
        model,
        created,
        first: Some(first),
        queue: VecDeque::new(),
        done: false,
    };
    if chat["stream"] == true {
        state
            .queue
            .push_back(state.chunk(json!({"role":"assistant","content":""}), None, None));
        let body = futures::stream::unfold(Some(state), |state| async move {
            let mut state = state?;
            match state.next().await {
                Ok(Some(bytes)) => Some((Ok(bytes), Some(state))),
                Ok(None) => None,
                Err(error) => Some((Err(error), None)),
            }
        });
        return Ok(FetchResponse {
            status: 200,
            headers: BTreeMap::from([
                ("content-type".into(), "text/event-stream".into()),
                ("cache-control".into(), "no-cache".into()),
            ]),
            body: Box::pin(body),
        });
    }
    let mut text = String::new();
    let mut thought = String::new();
    let mut calls = Vec::new();
    let mut end = None;
    loop {
        let event = match state.first.take() {
            Some(event) => Some(event),
            None => state.guard.run.next().await?,
        };
        match event{
        Some(Event::Text(s))=>text.push_str(&s),Some(Event::Reasoning(s))=>thought.push_str(&s),Some(Event::Tool(_,c))=>calls.push(json!({"id":c.id,"type":"function","function":{"name":c.name,"arguments":c.args.to_string()}})),
        Some(Event::Finish(stop,usage))=>{end=Some((stop,usage));break;},None=>break
    }
    }
    let (stop, usage) =
        end.ok_or_else(|| PluginError::transport("Cursor did not finish its reply."))?;
    let mut message = json!({"role":"assistant","content":text});
    if !thought.is_empty() {
        message["reasoning_content"] = json!(thought);
    }
    if !calls.is_empty() {
        message["tool_calls"] = json!(calls);
    }
    state.guard.run.delivered();
    state.guard.complete = true;
    Ok(FetchResponse::bytes(200,BTreeMap::from([("content-type".into(),"application/json".into())]),serde_json::to_vec(&json!({"id":state.id,"object":"chat.completion","created":state.created,"model":state.model,"choices":[{"index":0,"message":message,"finish_reason":stop}],"usage":usage})).map_err(|_|PluginError::transport("Cursor reply could not be encoded."))?))
}
