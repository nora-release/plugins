use super::cli;
use nora_provider_plugin_sdk::{PluginError, PluginResult};
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::Arc,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    net::{TcpListener, TcpStream},
    sync::{oneshot, Mutex},
    task::JoinSet,
};
pub(crate) const PREFIX: &str = "mcp__nora__";
const MAX_NAME_BYTES: usize = 64 - PREFIX.len();
const HASH_BYTES: usize = 12;
pub(crate) fn tool_name(original: &str) -> String {
    let suffix = original
        .rsplit_once('_')
        .map(|(_, tail)| {
            tail.len() == HASH_BYTES && tail.bytes().all(|byte| byte.is_ascii_hexdigit())
        })
        .unwrap_or(false);
    if !original.is_empty()
        && original.len() <= MAX_NAME_BYTES
        && !suffix
        && original
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"_-".contains(&byte))
    {
        return original.into();
    }
    let head = original
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || character == '_' || character == '-' {
                character
            } else {
                '_'
            }
        })
        .take(MAX_NAME_BYTES - HASH_BYTES - 1)
        .collect::<String>();
    format!("{head}_{}", &cli::hash(original)[..HASH_BYTES])
}
#[derive(Debug, Eq, PartialEq)]
pub(crate) enum ToolChoice {
    Auto,
    None,
    Required,
    Named(String),
}
pub(crate) fn choice(request: &Value) -> PluginResult<ToolChoice> {
    let Some(value) = request.get("tool_choice").filter(|value| !value.is_null()) else {
        return Ok(ToolChoice::Auto);
    };
    if value
        .get("disable_parallel_tool_use")
        .is_some_and(|value| !value.is_boolean())
    {
        return Err(PluginError::invalid("The Claude tool choice is invalid."));
    }
    match value["type"].as_str() {
        Some("auto") => Ok(ToolChoice::Auto),
        Some("none") => Ok(ToolChoice::None),
        Some("any") => Ok(ToolChoice::Required),
        Some("tool") => {
            let name = cli::text(value.get("name"), 4096)
                .ok_or_else(|| PluginError::invalid("The required Claude tool name is invalid."))?;
            if !request["tools"]
                .as_array()
                .into_iter()
                .flatten()
                .any(|tool| tool["name"] == name)
            {
                return Err(PluginError::invalid(
                    "The required Claude tool was not supplied.",
                ));
            }
            Ok(ToolChoice::Named(name.into()))
        }
        _ => Err(PluginError::invalid("The Claude tool choice is invalid.")),
    }
}
pub(crate) fn tool_choice_ask(request: &Value) -> PluginResult<String> {
    let mut ask = match choice(request)? {
        ToolChoice::Required => {
            "\nYou must call at least one available tool before answering.".into()
        }
        ToolChoice::Named(name) => {
            let native = request["tools"]
                .as_array()
                .into_iter()
                .flatten()
                .any(|tool| tool["name"] == name && super::web_search::is_search(tool));
            format!(
                "\nYou must call the {} tool.",
                if native {
                    "WebSearch".into()
                } else {
                    format!("{PREFIX}{}", tool_name(&name))
                }
            )
        }
        _ => String::new(),
    };
    if request.pointer("/tool_choice/disable_parallel_tool_use") == Some(&Value::Bool(true)) {
        ask.push_str("\nCall at most one tool at a time.");
    }
    Ok(ask)
}
pub(crate) fn tools(request: &Value) -> PluginResult<(Vec<Value>, BTreeMap<String, String>)> {
    super::web_search::requested(request)?;
    let choice = choice(request)?;
    let mut tools = Vec::new();
    let mut names = BTreeMap::new();
    for tool in request["tools"].as_array().into_iter().flatten() {
        if super::web_search::is_search(tool) {
            continue;
        }
        let original = cli::text(tool.get("name"), 4096)
            .ok_or_else(|| PluginError::invalid("The Claude tool name is invalid."))?;
        if tool.get("type").is_some_and(|kind| kind != "custom")
            && tool.get("input_schema").is_none()
        {
            return Err(PluginError::unsupported(
                "Claude Code built-in or server tools",
            ));
        }
        if choice == ToolChoice::None
            || matches!(&choice, ToolChoice::Named(required) if required != original)
        {
            continue;
        }
        let wire = tool_name(original);
        if names.insert(wire.clone(), original.into()).is_some() {
            return Err(PluginError::invalid("Claude tool names must be unique."));
        }
        tools.push(json!({"name":wire,"description":tool.get("description").and_then(Value::as_str).unwrap_or(original),"inputSchema":tool.get("input_schema").filter(|schema|schema.is_object()).cloned().unwrap_or_else(||json!({"type":"object","properties":{}}))}));
    }
    Ok((tools, names))
}
pub(crate) struct State {
    pub tools: Vec<Value>,
    pub names: BTreeMap<String, String>,
    pub pending: BTreeMap<String, oneshot::Sender<Value>>,
    pub early: BTreeMap<String, Value>,
    pub calls: BTreeMap<String, (String, Value)>,
    pub closed: bool,
}
pub(crate) struct Bridge {
    pub state: Arc<Mutex<State>>,
    pub path: PathBuf,
    task: Option<tokio::task::JoinHandle<()>>,
}
impl Drop for Bridge {
    fn drop(&mut self) {
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}
pub(crate) async fn bounded_line<R: tokio::io::AsyncBufRead + Unpin>(
    reader: &mut R,
    max: usize,
) -> PluginResult<Option<Vec<u8>>> {
    let mut bytes = Vec::new();
    loop {
        let chunk = reader.fill_buf().await.map_err(|_| cli::error())?;
        if chunk.is_empty() {
            return if bytes.is_empty() {
                Ok(None)
            } else {
                Err(PluginError::transport(
                    "The Claude protocol line was incomplete.",
                ))
            };
        }
        let end = chunk
            .iter()
            .position(|byte| *byte == b'\n')
            .map(|index| index + 1)
            .unwrap_or(chunk.len());
        if bytes.len().saturating_add(end) > max {
            return Err(PluginError::transport(
                "The Claude protocol line is too large.",
            ));
        }
        let finished = chunk[end - 1] == b'\n';
        bytes.extend_from_slice(&chunk[..end]);
        reader.consume(end);
        if finished {
            return Ok(Some(bytes));
        }
    }
}
impl Bridge {
    pub fn abort(&mut self) {
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
    pub async fn new(
        directory: &Path,
        tools: Vec<Value>,
        names: BTreeMap<String, String>,
    ) -> PluginResult<Self> {
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .await
            .map_err(|_| cli::error())?;
        let address = listener.local_addr().map_err(|_| cli::error())?;
        let token = cli::random();
        let path = directory.join("mcp-bridge.json");
        cli::write(
            &path,
            &serde_json::to_vec(&json!({"address":address.to_string(),"token":token}))
                .map_err(|_| cli::error())?,
        )?;
        let state = Arc::new(Mutex::new(State {
            tools,
            names,
            pending: BTreeMap::new(),
            early: BTreeMap::new(),
            calls: BTreeMap::new(),
            closed: false,
        }));
        let held = state.clone();
        let task = tokio::spawn(async move {
            let mut requests = JoinSet::new();
            loop {
                tokio::select! {accepted=listener.accept()=>{let Ok((stream,_))=accepted else{break;};let state=held.clone();let token=token.clone();requests.spawn(async move{let _=answer(stream,&token,state).await;});},_=requests.join_next(),if !requests.is_empty()=>{}}
            }
        });
        Ok(Self {
            state,
            path,
            task: Some(task),
        })
    }
    pub async fn deliver(&self, id: &str, value: Value) -> PluginResult<()> {
        let mut state = self.state.lock().await;
        if state.closed {
            return Err(cli::error());
        }
        if let Some(sender) = state.pending.remove(id) {
            sender.send(value).map_err(|_| cli::error())?;
        } else if state.calls.contains_key(id) {
            if state.early.insert(id.into(), value).is_some() {
                return Err(PluginError::invalid(
                    "A Claude tool result was already supplied.",
                ));
            }
        } else {
            return Err(PluginError::invalid(
                "This tool result belongs to a different Claude turn.",
            ));
        }
        Ok(())
    }
    pub async fn declare(&self, id: &str, name: &str, input: Value) -> PluginResult<String> {
        let wire = name.strip_prefix(PREFIX).ok_or_else(|| {
            PluginError::transport("Claude Code attempted a tool that Nora did not advertise.")
        })?;
        let mut state = self.state.lock().await;
        let original = state.names.get(wire).cloned().ok_or_else(|| {
            PluginError::transport("Claude Code attempted a tool that Nora did not advertise.")
        })?;
        if state.calls.len() >= 1024
            || state
                .calls
                .insert(id.into(), (wire.into(), input))
                .is_some()
        {
            return Err(PluginError::transport(
                "Claude Code reused a tool call identifier.",
            ));
        }
        Ok(original)
    }
    pub async fn replace_tools(&self, tools: Vec<Value>, names: BTreeMap<String, String>) {
        let mut state = self.state.lock().await;
        state.tools = tools;
        state.names = names;
    }
    pub async fn close(&self) {
        let mut state = self.state.lock().await;
        state.closed = true;
        state.pending.clear();
        state.early.clear();
    }
}
async fn answer(stream: TcpStream, token: &str, state: Arc<Mutex<State>>) -> PluginResult<()> {
    let (read, mut write) = stream.into_split();
    let mut reader = BufReader::new(read);
    let line = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        bounded_line(&mut reader, 16 * 1024 * 1024),
    )
    .await
    .map_err(|_| cli::error())??
    .ok_or_else(cli::error)?;
    let request: Value = serde_json::from_slice(&line).map_err(|_| cli::error())?;
    if request["token"].as_str() != Some(token) {
        return Err(cli::error());
    }
    let response = match request["method"].as_str() {
        Some("tools/list") => {
            let state = state.lock().await;
            json!({"tools":state.tools})
        }
        Some("tools/call") => {
            let id = cli::text(request.get("id"), 256)
                .ok_or_else(cli::error)?
                .to_owned();
            let name = cli::text(request.get("name"), 128)
                .ok_or_else(cli::error)?
                .to_owned();
            let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
            let mut held = loop {
                let held = state.lock().await;
                if held.closed || !held.names.contains_key(&name) {
                    return Err(cli::error());
                }
                if held.calls.contains_key(&id) {
                    break held;
                }
                drop(held);
                if tokio::time::Instant::now() >= deadline {
                    return Err(cli::error());
                }
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            };
            let (declared, input) = held.calls.get(&id).ok_or_else(cli::error)?;
            if declared != &name || input != &request["arguments"] {
                return Err(cli::error());
            }
            let result = if let Some(result) = held.early.remove(&id) {
                drop(held);
                result
            } else {
                if held.pending.contains_key(&id) {
                    return Err(cli::error());
                }
                let (send, receive) = oneshot::channel();
                held.pending.insert(id.clone(), send);
                drop(held);
                tokio::time::timeout(std::time::Duration::from_secs(300), receive)
                    .await
                    .map_err(|_| cli::error())?
                    .map_err(|_| cli::error())?
            };
            let state = state.lock().await;
            json!({"result":result,"tools":state.tools})
        }
        _ => return Err(cli::error()),
    };
    write
        .write_all(format!("{response}\n").as_bytes())
        .await
        .map_err(|_| cli::error())?;
    Ok(())
}
async fn callback(config: &Value, request: Value) -> PluginResult<Value> {
    let address = cli::text(config.get("address"), 128).ok_or_else(cli::error)?;
    let address = address
        .parse::<std::net::SocketAddr>()
        .map_err(|_| cli::error())?;
    if !address.ip().is_loopback() {
        return Err(cli::error());
    }
    let mut stream = TcpStream::connect(address)
        .await
        .map_err(|_| cli::error())?;
    let mut request = request;
    request["token"] = config["token"].clone();
    stream
        .write_all(format!("{request}\n").as_bytes())
        .await
        .map_err(|_| cli::error())?;
    let line = bounded_line(&mut BufReader::new(stream), 32 * 1024 * 1024)
        .await?
        .ok_or_else(cli::error)?;
    serde_json::from_slice(&line).map_err(|_| cli::error())
}
pub(crate) async fn helper(path: &Path) -> PluginResult<()> {
    let config = cli::read(path)?;
    let mut reader = BufReader::new(tokio::io::stdin());
    let out = Arc::new(Mutex::new(tokio::io::stdout()));
    let mut tasks = JoinSet::new();
    while let Some(line) = bounded_line(&mut reader, 16 * 1024 * 1024).await? {
        let request: Value = serde_json::from_slice(&line).map_err(|_| cli::error())?;
        if request.get("id").is_none() {
            continue;
        }
        let config = config.clone();
        let out = out.clone();
        tasks.spawn(async move{let result:PluginResult<Value>=async{match request["method"].as_str().unwrap_or_default(){"initialize"=>Ok(json!({"protocolVersion":"2025-06-18","capabilities":{"tools":{"listChanged":true}},"serverInfo":{"name":"nora","version":"1"}})),"tools/list"=>callback(&config,json!({"method":"tools/list"})).await,"tools/call"=>{let params=&request["params"];let id=cli::text(params.pointer("/_meta/claudecode~1toolUseId"),256).ok_or_else(||PluginError::invalid("Claude Code did not identify this tool call."))?;let result=callback(&config,json!({"method":"tools/call","id":id,"name":params["name"],"arguments":params.get("arguments").cloned().unwrap_or_else(||json!({}))})).await?;let notification=json!({"jsonrpc":"2.0","method":"notifications/tools/list_changed"});let mut out=out.lock().await;out.write_all(format!("{notification}\n").as_bytes()).await.map_err(|_|cli::error())?;out.flush().await.map_err(|_|cli::error())?;Ok(result["result"].clone())},_=>Err(PluginError::unsupported("MCP method"))}}.await;let response=match result{Ok(result)=>json!({"jsonrpc":"2.0","id":request["id"],"result":result}),Err(_)=>json!({"jsonrpc":"2.0","id":request["id"],"error":{"code":-32000,"message":"The Nora tool bridge request failed."}})};let mut out=out.lock().await;let _=out.write_all(format!("{response}\n").as_bytes()).await;let _=out.flush().await;});
    }
    while tasks.join_next().await.is_some() {}
    Ok(())
}
pub(crate) fn result(block: &Value) -> PluginResult<Value> {
    let mut content = Vec::new();
    match block.get("content") {
        Some(Value::String(text)) => content.push(json!({"type":"text","text":text})),
        Some(Value::Array(parts)) => {
            for part in parts {
                match part["type"].as_str() {
                    Some("text") => content.push(json!({"type":"text","text":part["text"]})),
                    Some("image") => {
                        if part.pointer("/source/type").and_then(Value::as_str) != Some("base64") {
                            return Err(PluginError::unsupported("URL-only Claude tool images"));
                        }
                        content.push(json!({"type":"image","data":part["source"]["data"],"mimeType":part["source"]["media_type"]}));
                    }
                    _ => return Err(PluginError::unsupported("Claude tool result content")),
                }
            }
        }
        None => {}
        _ => return Err(PluginError::invalid("The Claude tool result is invalid.")),
    }
    if content.is_empty() {
        content.push(json!({"type":"text","text":"(no output)"}));
    }
    Ok(json!({"content":content,"isError":block["is_error"]==true}))
}
#[cfg(test)]
#[path = "../test/mcp.test.rs"]
mod tests;
