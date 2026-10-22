use super::{
    cli,
    mcp::{self, Bridge},
    prompt,
    proxy::Proxy,
};
use nora_provider_plugin_sdk::{PluginError, PluginResult};
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    path::PathBuf,
    sync::Arc,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt, BufReader},
    sync::{mpsc, Mutex, OwnedMutexGuard},
};
pub(crate) struct Run {
    pub process: Option<cli::Process>,
    pub input: tokio::process::ChildStdin,
    pub output: mpsc::Receiver<PluginResult<Value>>,
    pub backlog: VecDeque<PluginResult<Value>>,
    pub bridge: Bridge,
    pub account: cli::Account,
    pub proxy: Arc<Proxy>,
    pub told: Vec<Value>,
    pub parked: BTreeSet<String>,
    pub turn: String,
    pub signature: String,
    pub dead: bool,
    pub idle: std::time::Instant,
    pub directory: PathBuf,
    tasks: Vec<tokio::task::JoinHandle<()>>,
}
impl Drop for Run {
    fn drop(&mut self) {
        self.cancel();
        for task in &self.tasks {
            task.abort();
        }
        let _ = std::fs::remove_dir_all(&self.directory);
    }
}
impl Run {
    pub fn cancel(&mut self) {
        self.dead = true;
        self.output.close();
        self.bridge.abort();
        if let Some(mut process) = self.process.take() {
            #[cfg(unix)]
            unsafe {
                if let Some(pid) = process.child.id() {
                    libc::kill(-(pid as i32), libc::SIGKILL);
                }
            }
            let _ = process.child.start_kill();
            if let Ok(runtime) = tokio::runtime::Handle::try_current() {
                runtime.spawn(async move {
                    let _ = process.child.wait().await;
                });
            }
        }
    }
    pub async fn user(&mut self, content: Vec<Value>) -> PluginResult<String> {
        let uuid = cli::random();
        let message = json!({"type":"user","message":{"role":"user","content":content},"session_id":"","parent_tool_use_id":null,"uuid":uuid});
        self.input
            .write_all(format!("{message}\n").as_bytes())
            .await
            .map_err(|_| cli::error())?;
        self.input.flush().await.map_err(|_| cli::error())?;
        Ok(uuid)
    }
    pub async fn queue(&mut self, content: Vec<Value>) -> PluginResult<()> {
        let id = self.user(content).await?;
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(2);
        loop {
            match tokio::time::timeout_at(deadline, self.output.recv()).await {
                Ok(Some(Ok(value)))
                    if value["type"] == "command_lifecycle"
                        && value["command_uuid"] == id
                        && value["state"] == "queued" =>
                {
                    return Ok(())
                }
                Ok(Some(value)) => self.backlog.push_back(value),
                _ => return Ok(()),
            }
        }
    }
}
#[derive(Default)]
pub(crate) struct Pool {
    runs: Mutex<BTreeMap<String, Arc<Mutex<Run>>>>,
}
fn signature(request: &Value, proxy: &Proxy) -> String {
    cli::hash(&json!({"model":request["model"],"system":request["system"],"tools":request["tools"],"format":request["output_config"],"thinking":request["thinking"],"proxy":proxy.env}).to_string())
}
impl Pool {
    pub async fn acquire(
        &self,
        account: cli::Account,
        request: &Value,
        metadata: &BTreeMap<String, String>,
        proxy: Arc<Proxy>,
    ) -> PluginResult<OwnedMutexGuard<Run>> {
        let conversation = metadata.get("sessionID").filter(|value| {
            !value.is_empty() && value.len() <= 1024 && !value.chars().any(char::is_control)
        });
        let provided_turn = metadata.get("turnID").filter(|value| {
            !value.is_empty() && value.len() <= 1024 && !value.chars().any(char::is_control)
        });
        let mut turn = provided_turn.cloned().unwrap_or_else(cli::random);
        let wanted = signature(request, &proxy);
        let incoming = request["messages"]
            .as_array()
            .ok_or_else(|| PluginError::invalid("Claude requires Anthropic messages."))?;
        let mut runs = self.runs.lock().await;
        let key = if let Some(conversation) = conversation {
            cli::hash(&format!(
                "{}\0{}\0{conversation}",
                account.identity,
                account.directory.display()
            ))
        } else {
            let candidates = runs
                .iter()
                .filter_map(|(key, run)| {
                    let run = run.try_lock().ok()?;
                    let length = run.told.len();
                    (!run.dead
                        && run.account.identity == account.identity
                        && run.account.directory == account.directory
                        && run.signature == wanted
                        && incoming.len() >= length
                        && prompt::canonical(&incoming[..length]) == prompt::canonical(&run.told))
                    .then(|| key.clone())
                })
                .collect::<Vec<_>>();
            if candidates.len() == 1 {
                candidates[0].clone()
            } else {
                cli::random()
            }
        };
        if let Some(run) = runs.get(&key) {
            let mut held = run.clone().try_lock_owned().map_err(|_| {
                PluginError::new("busy", "A request already owns this Claude conversation.")
            })?;
            if !held.dead
                && held.idle.elapsed() < std::time::Duration::from_secs(3600)
                && held.signature == wanted
            {
                let prefix = held.told.len();
                if incoming.len() >= prefix
                    && prompt::canonical(&incoming[..prefix]) == prompt::canonical(&held.told)
                {
                    let tail = &incoming[prefix..];
                    if !held.parked.is_empty() {
                        if provided_turn.is_none() {
                            turn = held.turn.clone();
                        }
                        if held.turn != turn {
                            return Err(PluginError::invalid(
                                "These Claude tool results belong to another turn.",
                            ));
                        }
                        let results = prompt::results(tail)?;
                        let ids = results
                            .iter()
                            .map(|(id, _)| id.clone())
                            .collect::<BTreeSet<_>>();
                        if ids != held.parked {
                            return Err(PluginError::invalid(
                                "Supply the exact pending Claude tool results for this turn.",
                            ));
                        }
                        let (tools, names) = mcp::tools(request)?;
                        held.bridge.replace_tools(tools, names).await;
                        let extra = tail
                            .iter()
                            .map(|message| {
                                let mut message = message.clone();
                                let remaining = prompt::parts(&message["content"])
                                    .into_iter()
                                    .filter(|part| part["type"] != "tool_result")
                                    .collect::<Vec<_>>();
                                message["content"] = json!(remaining);
                                message
                            })
                            .collect::<Vec<_>>();
                        let words = prompt::turn(&extra)?;
                        if !words.is_empty() {
                            // Queue user words before unblocking tools so Claude keeps their user role.
                            held.queue(words).await?;
                        }
                        for (id, result) in results {
                            held.bridge.deliver(&id, result).await?;
                        }
                        held.parked.clear();
                    } else {
                        if tail.is_empty() {
                            return Err(PluginError::invalid(
                                "This Claude turn was already completed.",
                            ));
                        }
                        held.turn = turn.clone();
                        held.user(prompt::turn(tail)?).await?;
                    }
                    held.told = incoming.clone();
                    return Ok(held);
                }
            }
            held.cancel();
        }
        runs.remove(&key);
        if runs.len() >= 16 {
            let evict = runs
                .iter()
                .find_map(|(key, run)| run.try_lock().ok().map(|_| key.clone()));
            if let Some(evict) = evict {
                runs.remove(&evict);
            } else {
                return Err(PluginError::new(
                    "busy",
                    "All Claude conversations are busy.",
                ));
            }
        }
        let mut run = start(account, request, turn, wanted, proxy).await?;
        run.user(prompt::initial(request)?).await?;
        run.told = incoming.clone();
        let run = Arc::new(Mutex::new(run));
        let held = run.clone().try_lock_owned().map_err(|_| cli::error())?;
        runs.insert(key, run);
        Ok(held)
    }
    pub async fn clear(&self) {
        let mut runs = self.runs.lock().await;
        for run in runs.values() {
            if let Ok(mut run) = run.try_lock() {
                run.cancel();
            }
        }
        runs.clear();
    }
}
async fn start(
    account: cli::Account,
    request: &Value,
    turn: String,
    signature: String,
    proxy: Arc<Proxy>,
) -> PluginResult<Run> {
    let directory = std::env::temp_dir().join(format!("nora-claude-run-{}", cli::random()));
    cli::directory(&directory)?;
    let (tools, names) = mcp::tools(request)?;
    let bridge = Bridge::new(&directory, tools, names).await?;
    let helper = std::env::current_exe().map_err(|_| cli::error())?;
    let mcp =
        json!({"mcpServers":{"nora":{"command":helper,"args":["claude-mcp-helper",bridge.path]}}});
    let model = cli::text(request.get("model"), 256)
        .filter(|model| !model.starts_with('-'))
        .ok_or_else(|| PluginError::invalid("The Claude model ID is invalid."))?;
    let mut args = [
        "-p",
        "--output-format",
        "stream-json",
        "--input-format",
        "stream-json",
        "--include-partial-messages",
        "--verbose",
        "--model",
        model,
        "--tools",
        "",
        "--strict-mcp-config",
        "--mcp-config",
        &mcp.to_string(),
        "--setting-sources",
        "",
        "--dangerously-skip-permissions",
        "--no-session-persistence",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect::<Vec<_>>();
    if let Some(effort) = cli::text(request.pointer("/output_config/effort"), 16)
        .filter(|effort| ["low", "medium", "high", "xhigh", "max"].contains(effort))
    {
        args.extend([
            "--effort".into(),
            effort.into(),
            "--thinking-display".into(),
            "summarized".into(),
        ]);
    }
    if request.pointer("/thinking/type").and_then(Value::as_str) == Some("disabled") {
        args.extend(["--thinking".into(), "disabled".into()]);
    }
    let mut process = cli::Process::spawn(
        &account.executable,
        &args,
        &directory,
        cli::env(&account.directory, &proxy, false),
    )?;
    let input = process.child.stdin.take().ok_or_else(cli::error)?;
    let stdout = process.child.stdout.take().ok_or_else(cli::error)?;
    let mut stderr = process.child.stderr.take().ok_or_else(cli::error)?;
    let (send, output) = mpsc::channel(64);
    let read = tokio::spawn(async move {
        let mut reader = BufReader::new(stdout);
        loop {
            let result = mcp::bounded_line(&mut reader, 16 * 1024 * 1024).await;
            match result {
                Ok(Some(line)) => {
                    let value = serde_json::from_slice(&line).map_err(|_| {
                        PluginError::transport("Claude Code returned an invalid protocol message.")
                    });
                    let failed = value.is_err();
                    if send.send(value).await.is_err() || failed {
                        return;
                    }
                }
                Ok(None) => {
                    let _ = send
                        .send(Err(PluginError::transport(
                            "Claude Code ended before completing the reply.",
                        )))
                        .await;
                    return;
                }
                Err(error) => {
                    let _ = send.send(Err(error)).await;
                    return;
                }
            }
        }
    });
    let drain = tokio::spawn(async move {
        let mut bytes = [0; 4096];
        while let Ok(size) = stderr.read(&mut bytes).await {
            if size == 0 {
                break;
            }
        }
    });
    Ok(Run {
        process: Some(process),
        input,
        output,
        backlog: VecDeque::new(),
        bridge,
        account,
        proxy,
        told: Vec::new(),
        parked: BTreeSet::new(),
        turn,
        signature,
        dead: false,
        idle: std::time::Instant::now(),
        directory,
        tasks: vec![read, drain],
    })
}
#[cfg(test)]
#[path = "../test/session.test.rs"]
mod tests;
