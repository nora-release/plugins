use super::{
    auth, network,
    protobuf::{self, Field, Pb},
    request::{self, Tool},
    transport::Transport,
};
use nora_provider_plugin_sdk::{PluginError, PluginResult};
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    time::Duration,
};

#[derive(Clone, Debug)]
pub(crate) struct Call {
    pub id: String,
    pub request_id: u64,
    pub exec: String,
    pub native: String,
    pub name: String,
    pub args: Value,
}
#[derive(Clone, Debug)]
pub(crate) enum Event {
    Text(String),
    Reasoning(String),
    Tool(usize, Call),
    Finish(&'static str, Value),
}
pub(crate) struct Run {
    pub account: String,
    pub controls: String,
    pub paused: bool,
    pub retained: bool,
    pub dead: bool,
    pub expires: tokio::time::Instant,
    transport: Transport,
    frames: protobuf::Frames,
    blobs: BTreeMap<Vec<u8>, Vec<u8>>,
    bootstrap: BTreeSet<Vec<u8>>,
    events: VecDeque<Event>,
    tools: Vec<Tool>,
    history: Vec<Value>,
    pub calls: Vec<Call>,
    expected: Option<u64>,
    answered: u64,
    text: String,
    reasoning: String,
    identity: String,
    segment: u64,
    exec_seen: BTreeMap<u64, Call>,
    native_seen: BTreeSet<String>,
    output_size: usize,
    step_output_size: usize,
    steps: u64,
    estimate: u64,
    accounting: crate::accounting::Book,
    terminal: bool,
    ended: bool,
    result_wait: Option<tokio::task::JoinHandle<()>>,
    since: tokio::time::Instant,
    batch_progress: tokio::time::Instant,
}
impl Drop for Run {
    fn drop(&mut self) {
        self.cancel();
    }
}
impl Run {
    pub fn cancel(&mut self) {
        self.dead = true;
        self.transport.close();
        self.accounting.abandon();
        if let Some(task) = self.result_wait.take() {
            task.abort();
        }
    }
    pub(crate) fn result_deadline(&self) -> tokio::time::Instant {
        self.since + Duration::from_secs(120)
    }
    pub(crate) async fn read_idle(&mut self) -> PluginResult<Option<Vec<u8>>> {
        self.transport.next().await
    }
    pub fn alive(&self) -> bool {
        !self.dead
            && tokio::time::Instant::now() < self.expires
            && (!self.paused || self.since.elapsed() < Duration::from_secs(120))
    }
    fn close_exec(id: u64) -> Vec<u8> {
        Pb::new()
            .bytes(5, Pb::new().bytes(1, Pb::new().num(1, id)))
            .0
    }
    async fn answer(&mut self, id: u64, exec: &str, kind: u32, body: Pb) -> PluginResult<()> {
        self.transport
            .writer
            .send(
                Pb::new()
                    .bytes(2, Pb::new().num(1, id).str(15, exec).bytes(kind, body))
                    .0,
            )
            .await?;
        self.transport.writer.send(Self::close_exec(id)).await
    }
    async fn refuse(&mut self, id: u64) -> PluginResult<()> {
        self.transport
            .writer
            .send(
                Pb::new()
                    .bytes(
                        5,
                        Pb::new().bytes(2, Pb::new().num(1, id).str(2, "not available")),
                    )
                    .0,
            )
            .await?;
        self.transport.writer.send(Self::close_exec(id)).await
    }
    fn seal(&mut self) -> PluginResult<()> {
        let Some(expected) = self.expected else {
            return Ok(());
        };
        let made = self.calls.len() as u64 + self.answered;
        if made > expected {
            return Err(PluginError::transport(
                "Cursor exceeded its declared tool batch.",
            ));
        }
        if made != expected {
            return Ok(());
        }
        if self.calls.is_empty() {
            self.expected = None;
            self.answered = 0;
            self.steps += 1;
            self.step_output_size = self.output_size;
            return Ok(());
        }
        self.paused = true;
        self.accounting.pending();
        self.since = tokio::time::Instant::now();
        self.events.push_back(Event::Finish(
            "tool_calls",
            json!({"prompt_tokens":0,"completion_tokens":0,"total_tokens":0}),
        ));
        if !self.retained {
            self.transport.close();
            return Ok(());
        }
        let writer = self.transport.writer.clone();
        let calls = self.calls.clone();
        self.result_wait = Some(tokio::spawn(async move {
            let deadline = tokio::time::Instant::now() + Duration::from_secs(120);
            let mut heartbeat = tokio::time::interval(Duration::from_secs(3));
            loop {
                tokio::select! {_ = tokio::time::sleep_until(deadline)=>{writer.fail(PluginError::transport("Cursor tool-result deadline expired."));writer.close();break;},_ = heartbeat.tick()=>{for call in &calls{if let Err(error)=writer.send(Pb::new().bytes(5,Pb::new().bytes(3,Pb::new().num(1,call.request_id))).0).await{writer.fail(error);return;}}}}
            }
        }));
        Ok(())
    }
    async fn exec(&mut self, bytes: &[u8]) -> PluginResult<()> {
        let fs = protobuf::fields(bytes)?;
        if fs
            .iter()
            .any(|f| f.id == 1 && (f.wire != 0 || f.n > u32::MAX as u64))
        {
            return Err(protobuf::malformed());
        }
        let id = protobuf::num(&fs, 1)?;
        if id > u32::MAX as u64 {
            return Err(protobuf::malformed());
        }
        let exec = protobuf::string(&fs, 15)?;
        let mut parsed: Option<Call> = None;
        let mut unavailable = false;
        for envelope in fs.iter().filter(|f| f.id == 11) {
            if envelope.wire != 2 {
                return Err(protobuf::malformed());
            }
            let (a, args) = protobuf::arguments(envelope.data)?;
            let full = protobuf::string(&a, 1)?;
            let bare = protobuf::string(&a, 5)?;
            let name = if bare.is_empty() {
                full.strip_prefix(&format!("{}-", request::NS))
                    .unwrap_or(&full)
                    .to_owned()
            } else {
                bare
            };
            let provider = protobuf::string(&a, 4)?;
            let server = protobuf::string(&a, 9)?;
            unavailable |= (!provider.is_empty() && provider != request::NS)
                || (!server.is_empty() && server != request::NS)
                || protobuf::num(&a, 7)? != 0
                || protobuf::field(&a, 6).is_some()
                || !self.tools.iter().any(|t| t.name == name)
                || (protobuf::string(&a, 5)?.is_empty()
                    && full != format!("{}-{name}", request::NS));
            let call = Call {
                id: String::new(),
                request_id: id,
                exec: exec.clone(),
                native: protobuf::string(&a, 3)?,
                name,
                args,
            };
            if let Some(prior) = &parsed {
                if prior.request_id != call.request_id
                    || prior.exec != call.exec
                    || prior.native != call.native
                    || prior.name != call.name
                    || prior.args != call.args
                {
                    return Err(PluginError::transport(
                        "Cursor repeated an ambiguous tool envelope.",
                    ));
                }
            }
            parsed = Some(call);
        }
        if let Some(mut call) = parsed.filter(|_| !unavailable) {
            if let Some(prior) = self.exec_seen.get(&id) {
                if prior.exec == call.exec
                    && prior.native == call.native
                    && prior.name == call.name
                    && prior.args == call.args
                {
                    return Ok(());
                }
                return Err(PluginError::transport(
                    "Cursor changed an existing tool call.",
                ));
            }
            if self.paused
                || self.terminal
                || !call.native.is_empty() && !self.native_seen.insert(call.native.clone())
            {
                return Err(PluginError::transport(
                    "Cursor reused a tool call or crossed a tool boundary.",
                ));
            }
            let index = self.calls.len();
            call.id = format!("call_cursor_{}_{}_{}", self.identity, self.segment, index);
            self.output_size += call.args.to_string().len();
            if self.output_size > 16 * 1024 * 1024 {
                return Err(PluginError::transport(
                    "Cursor output retention limit reached.",
                ));
            }
            self.exec_seen.insert(id, call.clone());
            self.batch_progress = tokio::time::Instant::now();
            self.calls.push(call.clone());
            self.events.push_back(Event::Tool(index, call));
            return self.seal();
        }
        if !unavailable && fs.iter().any(|f| f.id == 36) {
            let mut server = Pb::new()
                .str(1, request::NS)
                .str(2, request::NS)
                .str(7, "connected");
            for t in &self.tools {
                server = server.bytes(5, request::definition(t));
            }
            self.answer(
                id,
                &exec,
                36,
                Pb::new().bytes(1, Pb::new().bytes(1, server)),
            )
            .await?;
            self.answered += 1;
            return self.seal();
        }
        if !unavailable && fs.iter().any(|f| f.id == 10) {
            return self
                .answer(
                    id,
                    &exec,
                    10,
                    Pb::new().bytes(
                        1,
                        Pb::new().bytes(1, Pb::new().bytes(4, request::environment())),
                    ),
                )
                .await;
        }
        self.refuse(id).await?;
        self.answered += 1;
        self.seal()
    }
    async fn kv(&mut self, bytes: &[u8]) -> PluginResult<()> {
        let fs = protobuf::fields(bytes)?;
        let id = protobuf::num(&fs, 1)?;
        for f in &fs {
            if ![2, 3].contains(&f.id) {
                continue;
            }
            if f.wire != 2 {
                return Err(protobuf::malformed());
            }
            let body = protobuf::fields(f.data)?;
            let key = protobuf::field(&body, 1)
                .filter(|f| f.wire == 2)
                .map(|f| f.data)
                .ok_or_else(protobuf::malformed)?;
            let reply = if f.id == 2 {
                match self.blobs.get(key) {
                    Some(bytes) => Pb::new().bytes(1, bytes),
                    None => Pb::new().bytes(2, Pb::new().str(1, "blob not found")),
                }
            } else {
                let value = protobuf::field(&body, 2)
                    .filter(|f| f.wire == 2)
                    .map(|f| f.data)
                    .unwrap_or_default();
                if key.is_empty()
                    || self.bootstrap.contains(key)
                        && auth::hash(value)
                            != key.iter().map(|b| format!("{b:02x}")).collect::<String>()
                {
                    return Err(PluginError::transport(
                        "Cursor changed a content-addressed history blob.",
                    ));
                }
                let size = self.blobs.values().map(Vec::len).sum::<usize>()
                    - self.blobs.get(key).map_or(0, Vec::len)
                    + value.len();
                if size > 64 * 1024 * 1024 {
                    return Err(PluginError::transport(
                        "Cursor history retention limit reached.",
                    ));
                }
                self.blobs.insert(key.to_vec(), value.to_vec());
                Pb::new()
            };
            self.transport
                .writer
                .send(
                    Pb::new()
                        .bytes(3, Pb::new().num(1, id).bytes(f.id, reply))
                        .0,
                )
                .await?;
        }
        Ok(())
    }
    async fn finish(&mut self, mut usage: Value) -> PluginResult<()> {
        if self.terminal {
            return Err(PluginError::transport(
                "Cursor repeated its final usage snapshot.",
            ));
        }
        if !self.calls.is_empty() {
            return Err(PluginError::transport(
                "Cursor ended a turn with unresolved tool calls.",
            ));
        }
        if self.text.is_empty() {
            return Err(PluginError::transport(
                "Cursor ended with no answer or tool call.",
            ));
        }
        self.terminal = true;
        self.accounting.final_seen();
        self.transport.close();
        self.accounting.collect().await.attach(&mut usage);
        self.events.push_back(Event::Finish("stop", usage));
        Ok(())
    }
    async fn message(&mut self, bytes: &[u8]) -> PluginResult<()> {
        for m in protobuf::fields(bytes)? {
            match m.id {
                1 => {
                    for update in protobuf::fields(m.data)? {
                        let fields = protobuf::fields(update.data)?;
                        match update.id {
                            1 | 4 => {
                                if self.paused || self.terminal {
                                    return Err(PluginError::transport(
                                        "Cursor output crossed a sealed tool boundary.",
                                    ));
                                }
                                let text = protobuf::string(&fields, 1)?;
                                self.output_size += text.len();
                                if self.output_size > 16 * 1024 * 1024 {
                                    return Err(PluginError::transport(
                                        "Cursor output retention limit reached.",
                                    ));
                                }
                                if !text.is_empty() {
                                    self.batch_progress = tokio::time::Instant::now();
                                    if update.id == 1 {
                                        self.text.push_str(&text);
                                        self.events.push_back(Event::Text(text));
                                    } else {
                                        self.reasoning.push_str(&text);
                                        self.events.push_back(Event::Reasoning(text));
                                    }
                                }
                            }
                            14 => {
                                let usage = self.usage(&fields);
                                self.finish(usage).await?;
                            }
                            27 => {
                                let count = protobuf::num(&fields, 1)?;
                                if count == 0
                                    || count > 256
                                    || self.expected.is_some_and(|old| old != count)
                                {
                                    return Err(PluginError::transport(
                                        "Cursor declared an invalid or conflicting tool batch.",
                                    ));
                                }
                                if !self.paused {
                                    self.batch_progress = tokio::time::Instant::now();
                                    self.expected = Some(count);
                                    self.seal()?;
                                }
                            }
                            _ => {}
                        }
                    }
                }
                2 => self.exec(m.data).await?,
                3 => {
                    protobuf::fields(m.data)?;
                }
                4 => self.kv(m.data).await?,
                5 => {
                    for control in protobuf::fields(m.data)? {
                        if control.id == 1 {
                            let id = protobuf::num(&protobuf::fields(control.data)?, 1)?;
                            if self.calls.iter().any(|c| c.request_id == id) {
                                return Err(PluginError::transport(
                                    "Cursor cancelled a pending tool execution.",
                                ));
                            }
                        }
                    }
                }
                7 => {
                    return Err(PluginError::transport(
                        "Cursor requested an unsupported native interaction.",
                    ))
                }
                _ => {}
            }
        }
        Ok(())
    }
    pub async fn next(&mut self) -> PluginResult<Option<Event>> {
        if let Some(event) = self.events.pop_front() {
            return Ok(Some(event));
        }
        if self.ended {
            return Ok(None);
        }
        if !self.alive() {
            return Err(PluginError::transport("Cursor session expired."));
        }
        loop {
            let batch_pending = !self.calls.is_empty() || self.expected.is_some();
            let deadline = self.expires.min(if batch_pending {
                self.batch_progress + Duration::from_secs(30)
            } else {
                tokio::time::Instant::now() + Duration::from_secs(120)
            });
            let chunk = tokio::time::timeout_at(deadline, self.transport.next())
                .await
                .map_err(|_| {
                    PluginError::transport(if batch_pending {
                        "Cursor did not complete its declared tool batch."
                    } else {
                        "Cursor reply timed out."
                    })
                })??;
            let Some(chunk) = chunk else {
                self.frames.finish()?;
                if self.terminal {
                    return Ok(self.events.pop_front());
                }
                return Err(PluginError::transport(
                    "Cursor reply broke off before completing its turn or tool batch.",
                ));
            };
            if let Err(error) = self.receive(&chunk).await {
                if self.output_size > 0 {
                    return Err(PluginError::transport(error.message));
                }
                return Err(error);
            }
            if let Some(event) = self.events.pop_front() {
                return Ok(Some(event));
            }
        }
    }
    pub(crate) async fn receive(&mut self, chunk: &[u8]) -> PluginResult<()> {
        for (end, bytes) in self.frames.push(chunk)? {
            if end {
                let value: Value =
                    serde_json::from_slice(&bytes).map_err(|_| protobuf::malformed())?;
                if value.get("error").is_some() {
                    return Err(network::failure(502, &value));
                }
                if !self.terminal {
                    if self.paused {
                        return Err(PluginError::transport(
                            "Cursor ended a Run before receiving its pending tool results.",
                        ));
                    }
                    self.finish(self.usage(&[])).await?;
                }
                self.ended = true;
            } else {
                self.message(&bytes).await?;
            }
        }
        Ok(())
    }
    fn expected_assistant(&self) -> Value {
        let mut message = json!({"role":"assistant","content":self.text});
        if !self.reasoning.is_empty() {
            message["reasoning_content"] = json!(self.reasoning);
        }
        if !self.calls.is_empty() {
            message["tool_calls"]=json!(self.calls.iter().map(|c|json!({"id":c.id,"type":"function","function":{"name":c.name,"arguments":c.args.to_string()}})).collect::<Vec<_>>());
        }
        message
    }
    pub(crate) fn continuation(&self, chat: &Value) -> Option<Vec<Value>> {
        if !self.paused || !self.alive() {
            return None;
        }
        let messages = chat["messages"].as_array()?;
        let length = self.history.len();
        if messages.len() != length + 1 + self.calls.len() {
            return None;
        }
        if messages[..length]
            .iter()
            .map(normalize)
            .ne(self.history.iter().map(normalize))
            || normalize(&messages[length]) != normalize(&self.expected_assistant())
        {
            return None;
        }
        let mut seen = BTreeSet::new();
        let results = messages[length + 1..].to_vec();
        if results.iter().all(|m| {
            m["role"] == "tool"
                && m["tool_call_id"].as_str().is_some_and(|id| {
                    self.calls.iter().any(|c| c.id == id) && seen.insert(id.to_owned())
                })
        }) && seen.len() == self.calls.len()
        {
            Some(results)
        } else {
            None
        }
    }
    pub(crate) async fn resume(
        &mut self,
        chat: &Value,
        results: Vec<Value>,
        estimate: u64,
    ) -> PluginResult<()> {
        self.ended = false;
        // Encode every result before reserving any upload; an uncertain write is never replayed.
        let mut encoded = Vec::new();
        for result in results {
            let call = self
                .calls
                .iter()
                .find(|c| result["tool_call_id"] == c.id)
                .ok_or_else(protobuf::malformed)?;
            encoded.push((call.clone(), request::native_result(&result)?));
        }
        self.paused = false;
        if let Some(task) = self.result_wait.take() {
            task.abort();
        }
        self.segment += 1;
        self.steps += 1;
        self.step_output_size = self.output_size;
        for (call, body) in encoded {
            let message = Pb::new()
                .num(1, call.request_id)
                .str(15, &call.exec)
                .bytes(11, body);
            self.transport
                .writer
                .send(Pb::new().bytes(2, message).0)
                .await?;
            self.transport
                .writer
                .send(Self::close_exec(call.request_id))
                .await?;
        }
        self.history = chat["messages"]
            .as_array()
            .cloned()
            .ok_or_else(protobuf::malformed)?;
        self.calls.clear();
        self.expected = None;
        self.answered = 0;
        self.text.clear();
        self.reasoning.clear();
        self.events.clear();
        self.estimate = estimate;
        self.batch_progress = tokio::time::Instant::now();
        Ok(())
    }
    pub fn delivered(&mut self) {
        if self.terminal {
            self.dead = true;
            self.accounting.delivered();
        }
        self.ended = true;
    }
    pub fn usage_delivered(&mut self) {
        if self.terminal {
            self.accounting.delivered();
        }
    }
    fn usage(&self, fields: &[Field<'_>]) -> Value {
        split_usage(
            fields,
            self.estimate,
            self.output_size,
            self.steps,
            self.output_size.saturating_sub(self.step_output_size),
        )
    }
}
fn split_usage(
    fields: &[Field<'_>],
    estimate: u64,
    output_size: usize,
    steps: u64,
    step_size: usize,
) -> Value {
    let measured = |id| protobuf::field(fields, id).is_some();
    let measured_input = [1, 3, 4].into_iter().any(measured);
    let cr = protobuf::num(fields, 3).unwrap_or(0);
    let cw = protobuf::num(fields, 4).unwrap_or(0);
    let input = if measured_input {
        protobuf::num(fields, 1)
            .unwrap_or(0)
            .max(cr.saturating_add(cw))
    } else {
        estimate
    };
    let output = if measured(2) {
        protobuf::num(fields, 2).unwrap_or(0)
    } else {
        output_size.div_ceil(4) as u64
    };
    let uncached = input.saturating_sub(cr).saturating_sub(cw);
    let share = |total: u64, estimate: u64| {
        if total == 0 {
            0.0
        } else if steps <= 1 {
            1.0
        } else {
            ((estimate as f64).max(total as f64 / steps as f64) / total as f64).min(1.0)
        }
    };
    let input_share = share(input, if measured_input { estimate } else { input });
    let output_share = share(output, step_size.div_ceil(4) as u64);
    let portion = |value: u64, share: f64| ((value as f64 * share).round() as u64).min(value);
    let own_uncached = portion(uncached, input_share);
    let own_cached = portion(cr, input_share);
    let own_written = portion(cw, input_share);
    let own_input = own_uncached
        .saturating_add(own_cached)
        .saturating_add(own_written);
    let own_output = portion(output, output_share);
    let reasoning = portion(
        protobuf::num(fields, 5).unwrap_or(0).min(output),
        output_share,
    )
    .min(own_output);
    let earlier = crate::accounting::Counts {
        input: uncached - own_uncached,
        output: output - own_output,
        cached: cr - own_cached,
        written: cw - own_written,
    };
    let mut result = json!({"prompt_tokens":own_input,"completion_tokens":own_output,"total_tokens":own_input.saturating_add(own_output),"prompt_tokens_details":{"cached_tokens":own_cached,"cache_write_tokens":own_written},"completion_tokens_details":{"reasoning_tokens":reasoning},"cursor_accounting":if !measured_input||!measured(2){"estimated"}else if own_input != input || own_output != output {"allocated"}else{"measured"}});
    earlier.attach(&mut result);
    result
}
fn normalize(message: &Value) -> Value {
    if message["role"] != "assistant" {
        return message.clone();
    }
    let mut out = json!({"role":"assistant","content":request::text(&message["content"])});
    let thought = ["reasoning_content", "reasoning", "reasoning_text"]
        .into_iter()
        .find_map(|k| message[k].as_str().filter(|s| !s.is_empty()))
        .unwrap_or_default();
    if !thought.is_empty() {
        out["reasoning_content"] = json!(thought);
    }
    if let Some(calls) = message["tool_calls"].as_array() {
        out["tool_calls"] = json!(calls
            .iter()
            .map(|c| {
                let args = c["function"]["arguments"]
                    .as_str()
                    .and_then(|s| serde_json::from_str::<Value>(s).ok())
                    .unwrap_or_else(|| c["function"]["arguments"].clone());
                json!({"id":c["id"],"name":c["function"]["name"],"arguments":args})
            })
            .collect::<Vec<_>>());
    }
    out
}

impl Run {
    pub(crate) fn new(
        config: Open,
        chat: &Value,
        conv: request::Conversation,
        transport: Transport,
        blobs: BTreeMap<Vec<u8>, Vec<u8>>,
        accounting: crate::accounting::Book,
    ) -> Self {
        let bootstrap = blobs.keys().cloned().collect();
        Self {
            account: config.account,
            controls: config.controls,
            paused: false,
            retained: true,
            dead: false,
            expires: tokio::time::Instant::now() + Duration::from_secs(15 * 60),
            transport,
            frames: Default::default(),
            blobs,
            bootstrap,
            events: VecDeque::new(),
            tools: config.tools,
            history: chat["messages"].as_array().cloned().unwrap_or_default(),
            calls: Vec::new(),
            expected: None,
            answered: 0,
            text: String::new(),
            reasoning: String::new(),
            identity: auth::random(),
            segment: 0,
            exec_seen: BTreeMap::new(),
            native_seen: BTreeSet::new(),
            output_size: 0,
            step_output_size: 0,
            steps: 1,
            estimate: conv.estimate,
            accounting,
            terminal: false,
            ended: false,
            result_wait: None,
            since: tokio::time::Instant::now(),
            batch_progress: tokio::time::Instant::now(),
        }
    }
}

pub(crate) use crate::pool::{Guard, Open, Pool};

#[cfg(test)]
#[path = "../test/session.test.rs"]
mod tests;
