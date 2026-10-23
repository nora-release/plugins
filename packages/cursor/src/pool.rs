use super::{
    auth,
    models::Selection,
    network::Client,
    request::{self, Tool},
    session::Run,
    transport,
};
use futures::FutureExt;
use nora_provider_plugin_sdk::{PluginError, PluginResult};
use serde_json::Value;
use std::{
    collections::BTreeMap,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
};
use tokio::sync::{Mutex, Notify, OwnedMutexGuard};
struct Entry {
    run: Arc<Mutex<Run>>,
    account: String,
    controls: String,
    active: AtomicBool,
    retired: AtomicBool,
    wake: Notify,
    retained: bool,
}
impl Entry {
    fn idle(self: &Arc<Self>) {
        let entry = self.clone();
        tokio::spawn(async move {
            let mut run = entry.run.lock().await;
            while entry.wake.notified().now_or_never().is_some() {}
            if entry.retired.load(Ordering::Acquire) {
                run.cancel();
                return;
            }
            if entry.active.load(Ordering::Acquire) {
                return;
            }
            while run.paused
                && run.alive()
                && !entry.active.load(Ordering::Acquire)
                && !entry.retired.load(Ordering::Acquire)
            {
                let deadline = run.expires.min(run.result_deadline());
                let chunk = tokio::select! {
                    _=entry.wake.notified()=>break,
                    chunk=tokio::time::timeout_at(deadline,run.read_idle())=>chunk,
                };
                match chunk {
                    Ok(Ok(Some(chunk))) => {
                        if run.receive(&chunk).await.is_err() {
                            run.cancel();
                            break;
                        }
                    }
                    _ => {
                        run.cancel();
                        break;
                    }
                }
            }
            if entry.retired.load(Ordering::Acquire) {
                run.cancel();
            }
        });
    }
}
struct Reservation(Option<Arc<Entry>>);
impl Drop for Reservation {
    fn drop(&mut self) {
        if let Some(entry) = self.0.take() {
            entry.active.store(false, Ordering::Release);
            entry.wake.notify_one();
            entry.idle();
        }
    }
}
pub(crate) struct Guard {
    pub run: OwnedMutexGuard<Run>,
    pub complete: bool,
    owner: Arc<Entry>,
}
impl Drop for Guard {
    fn drop(&mut self) {
        if !self.complete || !self.owner.retained {
            self.run.cancel();
        }
        self.owner.active.store(false, Ordering::Release);
        if self.complete && self.owner.retained && self.run.paused && self.run.alive() {
            self.owner.idle();
        }
    }
}
#[derive(Default)]
pub(crate) struct Pool {
    runs: Mutex<BTreeMap<String, Arc<Entry>>>,
    accounting: Arc<crate::accounting::Ledger>,
    connector: transport::Connector,
}
impl Drop for Pool {
    fn drop(&mut self) {
        for entry in self.runs.get_mut().values() {
            entry.retired.store(true, Ordering::Release);
            entry.wake.notify_one();
            if let Ok(mut run) = entry.run.try_lock() {
                run.cancel();
            }
        }
    }
}
pub(crate) struct Open {
    pub client: Client,
    pub h2_client: Option<reqwest::Client>,
    pub token: String,
    pub version: String,
    pub base: String,
    pub account: String,
    pub controls: String,
    pub selector: String,
    pub selection: Selection,
    pub tools: Vec<Tool>,
    pub network_scope: String,
}
impl Pool {
    pub fn reset_transport(&self, network_scope: &str) {
        self.connector.reset(network_scope);
    }
    pub async fn acquire(&self, config: Open, chat: &Value) -> PluginResult<Guard> {
        let conv = request::conversation(chat, &config.tools)?;
        let mut runs = self.runs.lock().await;
        runs.retain(|_, entry| {
            entry
                .run
                .try_lock()
                .map(|mut run| {
                    if !run.alive() {
                        run.cancel();
                        false
                    } else {
                        true
                    }
                })
                .unwrap_or(true)
        });
        let candidates: Vec<_> = runs
            .values()
            .filter(|e| e.account == config.account && e.controls == config.controls)
            .cloned()
            .collect();
        drop(runs);
        // Wake the idle decoder before acquiring its Run. Active callers are never interrupted.
        for entry in candidates {
            if entry
                .active
                .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                .is_err()
            {
                continue;
            }
            let mut reservation = Reservation(Some(entry.clone()));
            entry.wake.notify_one();
            let mut guard = Guard {
                run: entry.run.clone().lock_owned().await,
                complete: true,
                owner: reservation.0.take().unwrap(),
            };
            if let Some(results) = guard.run.continuation(chat) {
                guard.complete = false;
                guard
                    .run
                    .resume(chat, results, conv.estimate)
                    .await
                    .map_err(|error| PluginError::transport(error.message))?;
                return Ok(guard);
            }
        }
        let runs = self.runs.lock().await;
        let retain = !(runs.len() >= 32
            || runs
                .values()
                .filter(|entry| entry.account == config.account)
                .count()
                >= 8);
        drop(runs);
        // A separate key leaves concurrent subagents isolated.
        let key = auth::random();
        let cid = if config.selector.is_empty() {
            request::uuid()
        } else {
            let s = &config.selector;
            format!(
                "{}-{}-4{}-8{}-{}",
                &s[..8],
                &s[8..12],
                &s[13..16],
                &s[17..20],
                &s[20..32]
            )
        };
        let (first, blobs) = request::build(&conv, &config.tools, &config.selection, &cid)?;
        let accounting = self.accounting.begin(&config, &cid, auth::now());
        let transport = self
            .connector
            .open(
                &config.network_scope,
                config.client.clone(),
                config.h2_client.as_ref(),
                &config.base,
                &config.token,
                &config.version,
                first,
            )
            .await?;
        let mut run = Run::new(config, chat, conv, transport, blobs, accounting);
        let mut runs = self.runs.lock().await;
        let retained = retain
            && !(runs.len() >= 32
                || runs
                    .values()
                    .filter(|entry| entry.account == run.account)
                    .count()
                    >= 8);
        run.retained = retained;
        let entry = Arc::new(Entry {
            account: run.account.clone(),
            controls: run.controls.clone(),
            run: Arc::new(Mutex::new(run)),
            active: AtomicBool::new(true),
            retired: AtomicBool::new(false),
            wake: Notify::new(),
            retained,
        });
        let guard = entry.run.clone().lock_owned().await;
        if retained {
            runs.insert(key, entry.clone());
        }
        Ok(Guard {
            run: guard,
            complete: false,
            owner: entry,
        })
    }
}
