use super::{
    cli, cli_sessions,
    mcp::{self, Bridge},
    prompt,
    proxy::Proxy,
};
use nora_provider_plugin_sdk::{PluginError, PluginResult};
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
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
    pub web_search: bool,
    pub output_schema: Option<Value>,
    pub cli_history: super::cli_messages::History,
    pub dead: bool,
    pub idle: std::time::Instant,
    pub directory: PathBuf,
    work: PathBuf,
    session: Option<cli_sessions::Session>,
    pub(super) checkpoint: Option<super::session_control::Checkpoint>,
    pub(super) recovering: Arc<AtomicBool>,
    pub(super) retry_key: Arc<std::sync::Mutex<Option<String>>>,
    pub(super) effort: String,
    pub(super) rewound: bool,
    tasks: Vec<tokio::task::JoinHandle<()>>,
    pub(crate) learned: Option<super::learned_usage::Cache>,
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
    pub fn capture_session(&mut self, value: &Value) -> PluginResult<()> {
        if value["type"] != "system" || value["subtype"] != "init" {
            return Ok(());
        }
        let Some(id) = value["session_id"].as_str() else {
            return Ok(());
        };
        if let Some(session) = &self.session {
            if session.id != id {
                return Err(PluginError::transport(
                    "Claude Code changed its session ID unexpectedly.",
                ));
            }
            return Ok(());
        }
        self.session = Some(cli_sessions::Session::capture(
            &self.account,
            &self.work,
            id,
        )?);
        Ok(())
    }
    async fn initialize_resume(&mut self) -> PluginResult<()> {
        let id = cli::random();
        let request =
            json!({"type":"control_request","request_id":id,"request":{"subtype":"initialize"}});
        self.input
            .write_all(format!("{request}\n").as_bytes())
            .await
            .map_err(|_| cli::error())?;
        self.input.flush().await.map_err(|_| cli::error())?;
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(20);
        loop {
            let value = tokio::time::timeout_at(deadline, self.output.recv())
                .await
                .map_err(|_| cli::error())?
                .ok_or_else(cli::error)??;
            match value["type"].as_str() {
                Some("control_response") if value["response"]["request_id"] == id => {
                    if value["response"]["subtype"] == "success" {
                        return Ok(());
                    }
                    return Err(cli::error());
                }
                Some("system") => self.capture_session(&value)?,
                Some("rate_limit_event" | "command_lifecycle") => {}
                _ => return Err(cli::error()),
            }
        }
    }
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
const RUN_LIMIT: usize = 16;
const SHELF_LIMIT: usize = 64;
const IDLE_LONGEST: std::time::Duration = std::time::Duration::from_secs(3600);
struct Saved {
    owner: String,
    login: String,
    signature: String,
    told: Vec<Value>,
    history: super::cli_messages::History,
    idle: std::time::Instant,
    session: cli_sessions::Session,
}
impl Saved {
    fn matches(&self, owner: &str, login: &str, wanted: &str, incoming: &[Value]) -> bool {
        self.owner == owner
            && self.login == login
            && self.signature == wanted
            && self.idle.elapsed() < IDLE_LONGEST
            && incoming.len() > self.told.len()
            && prompt::canonical(&incoming[..self.told.len()]) == prompt::canonical(&self.told)
            && self.session.exists()
    }
}
struct Slot {
    run: Arc<Mutex<Run>>,
    recovering: Arc<AtomicBool>,
    retry_key: Arc<std::sync::Mutex<Option<String>>>,
}
impl std::ops::Deref for Slot {
    type Target = Mutex<Run>;
    fn deref(&self) -> &Self::Target {
        &self.run
    }
}
#[derive(Default)]
struct PoolState {
    runs: BTreeMap<String, Slot>,
    shelf: BTreeMap<String, Saved>,
}
impl PoolState {
    fn expire(&mut self) {
        self.shelf
            .retain(|_, saved| saved.idle.elapsed() < IDLE_LONGEST);
        self.runs.retain(|_, run| {
            let Ok(held) = run.try_lock() else {
                return true;
            };
            !held.dead && held.idle.elapsed() < IDLE_LONGEST
        });
    }
    fn shelve(&mut self, key: String, mut run: OwnedMutexGuard<Run>) {
        if !run.dead
            && !run.rewound
            && run.parked.is_empty()
            && run.idle.elapsed() < IDLE_LONGEST
            && run
                .session
                .as_ref()
                .is_some_and(cli_sessions::Session::exists)
        {
            let saved = Saved {
                owner: cli_sessions::owner(&run.account),
                login: run.account.session_binding.clone(),
                signature: run.signature.clone(),
                told: std::mem::take(&mut run.told),
                history: std::mem::take(&mut run.cli_history),
                idle: run.idle,
                session: run.session.take().unwrap(),
            };
            self.shelf.insert(key, saved);
            while self.shelf.len() > SHELF_LIMIT {
                let oldest = self
                    .shelf
                    .iter()
                    .min_by_key(|(_, saved)| saved.idle)
                    .map(|(key, _)| key.clone())
                    .unwrap();
                self.shelf.remove(&oldest);
            }
        }
        run.cancel();
    }
}
#[derive(Default)]
pub(crate) struct Pool {
    state: Arc<Mutex<PoolState>>,
    reaper: std::sync::Mutex<Option<tokio::task::JoinHandle<()>>>,
}
impl Drop for Pool {
    fn drop(&mut self) {
        if let Ok(reaper) = self.reaper.get_mut() {
            if let Some(reaper) = reaper.take() {
                reaper.abort();
            }
        }
    }
}
fn signature(request: &Value, proxy: &Proxy) -> String {
    let tools = request["tools"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|tool| !super::web_search::is_search(tool))
        .collect::<Vec<_>>();
    let web_search = request["tools"]
        .as_array()
        .into_iter()
        .flatten()
        .any(super::web_search::is_search);
    let mut format = request["output_config"].clone();
    if let Some(format) = format.as_object_mut() {
        format.remove("effort");
    }
    if format.as_object().is_some_and(|format| format.is_empty()) {
        format = Value::Null;
    }
    cli::hash(&json!({"model":request["model"],"system":request["system"],"tools":tools,"webSearch":web_search,"toolChoice":request["tool_choice"],"format":format,"thinking":request["thinking"],"proxy":proxy.env}).to_string())
}
impl Pool {
    pub(crate) async fn parked(&self, credentials: &Value) -> u32 {
        let mut pool = self.state.lock().await;
        pool.expire();
        let identity = credentials
            .get("accountIdentity")
            .or_else(|| credentials.pointer("/extras/accountIdentity"));
        let account_id = credentials
            .get("accountId")
            .and_then(Value::as_str)
            .filter(|id| !id.is_empty());
        pool.runs
            .values()
            .filter_map(|slot| slot.try_lock().ok())
            .filter(|run| {
                if run.dead || run.parked.is_empty() {
                    return false;
                }
                let saved = run.account.saved();
                if let Some(identity) = identity {
                    return saved.get("accountIdentity") == Some(identity);
                }
                account_id.is_some_and(|id| run.account.identity == id)
            })
            .count()
            .min(u32::MAX as usize) as u32
    }

    pub async fn acquire(
        &self,
        account: cli::Account,
        request: &Value,
        metadata: &BTreeMap<String, String>,
        proxy: Arc<Proxy>,
        root: &Path,
    ) -> PluginResult<OwnedMutexGuard<Run>> {
        mcp::choice(request)?;
        super::structured_output::schema(request)?;
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
        self.start_reaper();
        let mut pool = self.state.lock().await;
        pool.expire();
        let owner = cli_sessions::owner(&account);
        let key = if let Some(conversation) = conversation {
            cli::hash(&format!("{owner}\0{conversation}"))
        } else {
            let mut candidates = pool
                .runs
                .iter()
                .filter_map(|(key, run)| {
                    if run.recovering.load(Ordering::Acquire) {
                        let wanted = super::session_control::retry_key(
                            &owner,
                            &account.session_binding,
                            &wanted,
                            incoming,
                        );
                        return run.retry_key.lock().ok().and_then(|saved| {
                            (saved.as_deref() == Some(&wanted)).then(|| key.clone())
                        });
                    }
                    let run = run.try_lock().ok()?;
                    let length = run.told.len();
                    (!run.dead
                        && cli_sessions::owner(&run.account) == owner
                        && run.account.session_binding == account.session_binding
                        && run.signature == wanted
                        && incoming.len() >= length
                        && prompt::canonical(&incoming[..length]) == prompt::canonical(&run.told))
                    .then(|| key.clone())
                })
                .collect::<Vec<_>>();
            candidates.extend(
                pool.shelf
                    .iter()
                    .filter(|(_, saved)| {
                        saved.matches(&owner, &account.session_binding, &wanted, incoming)
                    })
                    .map(|(key, _)| key.clone()),
            );
            if candidates.len() == 1 {
                candidates[0].clone()
            } else {
                cli::random()
            }
        };
        if let Some(slot) = pool.runs.get(&key) {
            let run = slot.run.clone();
            let recovering = slot.recovering.load(Ordering::Acquire);
            drop(pool);
            let mut held = if recovering {
                tokio::time::timeout(std::time::Duration::from_secs(11), run.clone().lock_owned())
                    .await
                    .map_err(|_| cli::error())?
            } else {
                run.clone().try_lock_owned().map_err(|_| {
                    PluginError::new("busy", "A request already owns this Claude conversation.")
                })?
            };
            if !held.dead
                && held.idle.elapsed() < IDLE_LONGEST
                && held.account.session_binding == account.session_binding
                && held.signature == wanted
                && held.set_effort(request).await.is_ok()
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
                        let words = prompt::turn(tail)?;
                        held.begin_turn(words, turn.clone()).await?;
                    }
                    held.told = incoming.clone();
                    held.account = account;
                    return Ok(held);
                }
            }
            held.cancel();
            drop(held);
            pool = self.state.lock().await;
            if pool
                .runs
                .get(&key)
                .is_some_and(|slot| !Arc::ptr_eq(&slot.run, &run))
            {
                return Err(PluginError::new(
                    "busy",
                    "Another request replaced this Claude conversation.",
                ));
            }
        }
        pool.runs.remove(&key);
        if pool.runs.len() >= RUN_LIMIT {
            let evict = pool
                .runs
                .iter()
                .filter_map(|(key, run)| {
                    let held = run.try_lock().ok()?;
                    held.parked.is_empty().then(|| (key.clone(), held.idle))
                })
                .min_by_key(|(_, idle)| *idle)
                .map(|(key, _)| key);
            if let Some(evict) = evict {
                let run = pool.runs.remove(&evict).unwrap();
                let held = run.run.try_lock_owned().map_err(|_| cli::error())?;
                pool.shelve(evict, held);
            } else {
                return Err(PluginError::new(
                    "busy",
                    "All Claude conversations are busy.",
                ));
            }
        }
        let from = pool
            .shelf
            .remove(&key)
            .filter(|saved| saved.matches(&owner, &account.session_binding, &wanted, incoming));
        let prefix = from.as_ref().map_or(0, |saved| saved.told.len());
        let resumed = from.is_some();
        let work = cli_sessions::work_directory(&account, root)?;
        let learned = super::learned_usage::Cache::for_credentials(root, &account.saved());
        let result = start(
            account.clone(),
            request,
            turn.clone(),
            wanted.clone(),
            proxy.clone(),
            work.clone(),
            from,
            learned.clone(),
        )
        .await;
        let (mut run, prefix) = match result {
            Ok(run) => (run, prefix),
            Err(_) if resumed => (
                start(account, request, turn, wanted, proxy, work, None, learned).await?,
                0,
            ),
            Err(error) => return Err(error),
        };
        let words = if prefix > 0 {
            prompt::turn(&incoming[prefix..])?
        } else {
            prompt::initial(request)?
        };
        if prefix > 0 {
            run.told = incoming[..prefix].to_vec();
            let turn = run.turn.clone();
            run.begin_turn(words, turn).await?;
        } else {
            run.user(words).await?;
        }
        run.told = incoming.clone();
        let run = Arc::new(Mutex::new(run));
        let held = run.clone().try_lock_owned().map_err(|_| cli::error())?;
        let recovering = held.recovering.clone();
        let retry_key = held.retry_key.clone();
        pool.runs.insert(
            key,
            Slot {
                run,
                recovering,
                retry_key,
            },
        );
        Ok(held)
    }
    fn start_reaper(&self) {
        let Ok(mut reaper) = self.reaper.lock() else {
            return;
        };
        if reaper.is_some() {
            return;
        }
        let state = Arc::downgrade(&self.state);
        *reaper = Some(tokio::spawn(async move {
            loop {
                tokio::time::sleep(std::time::Duration::from_secs(60)).await;
                let Some(state) = state.upgrade() else {
                    return;
                };
                state.lock().await.expire();
            }
        }));
    }
    pub async fn clear(&self) {
        let mut pool = self.state.lock().await;
        for run in pool.runs.values() {
            if let Ok(mut run) = run.try_lock() {
                run.cancel();
            }
        }
        pool.runs.clear();
        pool.shelf.clear();
    }
}
async fn start(
    account: cli::Account,
    request: &Value,
    turn: String,
    signature: String,
    proxy: Arc<Proxy>,
    work: PathBuf,
    from: Option<Saved>,
    learned: Option<super::learned_usage::Cache>,
) -> PluginResult<Run> {
    let (tools, names) = mcp::tools(request)?;
    let web_search = super::web_search::requested(request)?;
    let output_schema = super::structured_output::schema(request)?;
    let model = cli::text(request.get("model"), 256)
        .filter(|model| !model.starts_with('-'))
        .ok_or_else(|| PluginError::invalid("The Claude model ID is invalid."))?;
    let directory = std::env::temp_dir().join(format!("nora-claude-run-{}", cli::random()));
    cli::directory(&directory)?;
    let bridge = Bridge::new(&directory, tools, names).await?;
    let helper = std::env::current_exe().map_err(|_| cli::error())?;
    let mcp =
        json!({"mcpServers":{"nora":{"command":helper,"args":["claude-mcp-helper",bridge.path]}}});
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
        if web_search { "WebSearch" } else { "" },
        "--strict-mcp-config",
        "--mcp-config",
        &mcp.to_string(),
        "--setting-sources",
        "",
        "--dangerously-skip-permissions",
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
    if let Some(schema) = &output_schema {
        args.extend(["--json-schema".into(), schema.to_string()]);
    }
    if let Some(from) = &from {
        args.extend(["--resume".into(), from.session.id.clone()]);
    }
    let environment = cli::env(&account.home, &proxy, false);
    let mut process = cli::Process::spawn(&account.executable, &args, &work, environment)?;
    let input = process.child.stdin.take().ok_or_else(cli::error)?;
    let stdout = process.child.stdout.take().ok_or_else(cli::error)?;
    let mut stderr = process.child.stderr.take().ok_or_else(cli::error)?;
    let (send, output) = mpsc::channel(64);
    let observed = learned.clone();
    let read = tokio::spawn(async move {
        let mut reader = BufReader::new(stdout);
        loop {
            let result = mcp::bounded_line(&mut reader, 16 * 1024 * 1024).await;
            match result {
                Ok(Some(line)) => {
                    let value: PluginResult<Value> = serde_json::from_slice(&line).map_err(|_| {
                        PluginError::transport("Claude Code returned an invalid protocol message.")
                    });
                    if let (Some(cache), Ok(event)) = (&observed, &value) {
                        if event["type"] == "rate_limit_event" {
                            cache.note(event.clone()).await;
                        }
                    }
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
    let resumed = from.is_some();
    let (session, history) = from
        .map_or((None, super::cli_messages::History::default()), |saved| {
            (Some(saved.session), saved.history)
        });
    let mut run = Run {
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
        web_search,
        output_schema,
        cli_history: history,
        dead: false,
        idle: std::time::Instant::now(),
        directory,
        work,
        session,
        checkpoint: None,
        recovering: Arc::new(AtomicBool::new(false)),
        retry_key: Arc::new(std::sync::Mutex::new(None)),
        effort: super::session_control::effort(request),
        rewound: false,
        tasks: vec![read, drain],
        learned,
    };
    if resumed {
        run.initialize_resume().await?;
    }
    Ok(run)
}
#[cfg(test)]
#[path = "../test/session.test.rs"]
mod tests;
