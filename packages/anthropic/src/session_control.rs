use super::{cli, cli_messages, session::Run};
use nora_provider_plugin_sdk::{PluginError, PluginResult};
use serde_json::{json, Value};
use std::sync::atomic::Ordering;
use tokio::{io::AsyncWriteExt, sync::OwnedMutexGuard};

pub(crate) struct Checkpoint {
    uuid: String,
    told: Vec<Value>,
    history: cli_messages::History,
    turn: String,
}

pub(crate) fn effort(request: &Value) -> String {
    cli::text(request.pointer("/output_config/effort"), 16)
        .filter(|value| ["low", "medium", "high", "xhigh", "max"].contains(value))
        .unwrap_or_default()
        .into()
}

pub(super) fn retry_key(owner: &str, login: &str, signature: &str, incoming: &[Value]) -> String {
    cli::hash(&format!(
        "{owner}\0{login}\0{signature}\0{}",
        super::prompt::canonical(incoming)
    ))
}

impl Run {
    async fn control(&mut self, request: Value, rewinding: bool) -> PluginResult<()> {
        let id = cli::random();
        let request = json!({"type":"control_request","request_id":id,"request":request});
        let operation = async {
            self.input
                .write_all(format!("{request}\n").as_bytes())
                .await
                .map_err(|_| cli::error())?;
            self.input.flush().await.map_err(|_| cli::error())?;
            self.backlog.clear();
            loop {
                let value = self.output.recv().await.ok_or_else(cli::error)??;
                if value["type"] == "system" {
                    self.capture_session(&value)?;
                }
                if value["type"] != "control_response" || value["response"]["request_id"] != id {
                    continue;
                }
                if value["response"]["subtype"] != "success"
                    || rewinding && value["response"]["response"]["rewound"] != true
                {
                    return Err(PluginError::transport(
                        "Claude Code could not update this conversation.",
                    ));
                }
                return Ok(());
            }
        };
        tokio::time::timeout(std::time::Duration::from_secs(10), operation)
            .await
            .map_err(|_| cli::error())?
    }

    pub(crate) async fn set_effort(&mut self, request: &Value) -> PluginResult<()> {
        let next = effort(request);
        if self.effort == next {
            return Ok(());
        }
        self.control(
            json!({"subtype":"apply_flag_settings","settings":{"effortLevel":next}}),
            false,
        )
        .await?;
        self.effort = next;
        Ok(())
    }

    pub(crate) async fn begin_turn(
        &mut self,
        content: Vec<Value>,
        turn: String,
    ) -> PluginResult<()> {
        let uuid = self.user(content).await?;
        self.checkpoint = Some(Checkpoint {
            uuid,
            told: self.told.clone(),
            history: self.cli_history.clone(),
            turn: self.turn.clone(),
        });
        self.turn = turn;
        Ok(())
    }

    async fn rewind(&mut self) -> PluginResult<()> {
        let checkpoint = self.checkpoint.take().ok_or_else(cli::error)?;
        self.control(json!({"subtype":"rewind_conversation","target_message_uuid":checkpoint.uuid,"interrupt_if_running":true}), true).await?;
        self.told = checkpoint.told;
        self.cli_history = checkpoint.history;
        self.turn = checkpoint.turn;
        self.parked.clear();
        self.idle = std::time::Instant::now();
        // Claude's on-disk transcript may still contain the interrupted turn.
        // A rewound process can be reused live but cannot be put on the shelf.
        self.rewound = true;
        let mut bridge = self.bridge.state.lock().await;
        bridge.early.clear();
        bridge.calls.clear();
        Ok(())
    }
}

pub(crate) fn cancel(mut run: OwnedMutexGuard<Run>) {
    let pending = run
        .bridge
        .state
        .try_lock()
        .map_or(true, |bridge| !bridge.pending.is_empty());
    if run.dead || run.checkpoint.is_none() || !run.parked.is_empty() || pending {
        run.cancel();
        return;
    }
    let Ok(runtime) = tokio::runtime::Handle::try_current() else {
        run.cancel();
        return;
    };
    let recovering = run.recovering.clone();
    let retry = retry_key(
        &super::cli_sessions::owner(&run.account),
        &run.account.session_binding,
        &run.signature,
        &run.told,
    );
    let Ok(mut marker) = run.retry_key.lock() else {
        run.cancel();
        return;
    };
    *marker = Some(retry);
    drop(marker);
    recovering.store(true, Ordering::Release);
    runtime.spawn(async move {
        if run.rewind().await.is_err() {
            run.cancel();
        }
        recovering.store(false, Ordering::Release);
    });
}
