use crate::OAuthCallbackInput;
use nora_provider_plugin_sdk::{PluginError, PluginResult};
use serde_json::{json, Value};
use tokio::task::JoinHandle;

/// One exchange task shared by the listener and pasted callback input.
/// The task remains alive between short polling calls; dropping it cancels both.
pub struct BrowserSignIn<T> {
    pub callback: Option<OAuthCallbackInput>,
    pub task: JoinHandle<PluginResult<T>>,
    finished: bool,
}

impl<T> BrowserSignIn<T> {
    pub fn new(task: JoinHandle<PluginResult<T>>, callback: Option<OAuthCallbackInput>) -> Self {
        Self {
            callback,
            task,
            finished: false,
        }
    }

    pub fn pending_reply(&self, session: &str, params: &Value) -> PluginResult<Option<Value>> {
        let Some(callback) = &self.callback else {
            return Ok(None);
        };
        if self.task.is_finished() {
            return Ok(None);
        }
        let mut waiting = json!({"session":session,"pending":true});
        if let Some(raw) = params.get("callbackURL") {
            let result = raw
                .as_str()
                .ok_or_else(|| "Paste the complete callback address from this sign-in.".to_owned())
                .and_then(|raw| callback.submit(raw).map_err(|error| error.to_string()));
            if let Err(error) = result {
                waiting["inputError"] = json!(error);
                return Ok(Some(waiting));
            }
        }
        Ok(self.waiting_reply(session, params["poll"] == true))
    }

    pub fn waiting_reply(&self, session: &str, polling: bool) -> Option<Value> {
        (polling && !self.task.is_finished()).then(|| json!({"session":session,"pending":true}))
    }

    pub fn completion_started(&self) -> bool {
        self.finished
    }

    pub async fn finish(&mut self) -> PluginResult<T> {
        if std::mem::replace(&mut self.finished, true) {
            return Err(PluginError::authentication(
                "This sign-in has already finished.",
            ));
        }
        (&mut self.task)
            .await
            .map_err(|_| PluginError::authentication("The sign-in was interrupted."))?
    }
}

impl<T> Drop for BrowserSignIn<T> {
    fn drop(&mut self) {
        self.task.abort();
    }
}
