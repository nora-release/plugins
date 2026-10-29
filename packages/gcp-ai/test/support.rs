use crate::{cli::Gcloud, settings::Settings};
use async_trait::async_trait;
use nora_ai::{HttpClient, HttpClientError, HttpRequest, HttpStreamingResponse};
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    path::PathBuf,
    sync::{Arc, Mutex},
    time::Duration,
};

pub struct Directory(pub PathBuf);
impl Directory {
    pub fn new() -> Self {
        let path = std::env::temp_dir().join(format!("nora-gcp-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }
    pub fn settings(&self) -> Settings {
        let env = BTreeMap::from([
            ("HOME".into(), self.0.to_string_lossy().into_owned()),
            (
                "XDG_CACHE_HOME".into(),
                self.0.join("cache").to_string_lossy().into_owned(),
            ),
        ]);
        let mut settings=Settings::resolve_with_env(&json!({"project":"fixture-project","account":"person@example.invalid","authMode":"adc"}),None,&self.0,&env).unwrap();
        settings.cloudsdk_config = Some(self.0.join("adc-source"));
        settings
    }
    #[cfg(unix)]
    pub fn cli(&self, extra: BTreeMap<String, String>) -> Gcloud {
        use std::os::unix::fs::PermissionsExt;
        let path = self.0.join("gcloud");
        std::fs::write(&path, include_bytes!("fixtures/gcloud.sh")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
        let mut env = BTreeMap::from([
            ("HOME".into(), self.0.to_string_lossy().into_owned()),
            ("PATH".into(), "/usr/bin:/bin".into()),
            (
                "FAKE_GCLOUD_LOG".into(),
                self.0.join("commands").to_string_lossy().into_owned(),
            ),
            ("FAKE_GCLOUD_PROJECT".into(), "fixture-project".into()),
            (
                "FAKE_GCLOUD_ACCOUNT".into(),
                "person@example.invalid".into(),
            ),
        ]);
        env.extend(extra);
        Gcloud::new(path, env)
    }
    pub fn commands(&self) -> String {
        std::fs::read_to_string(self.0.join("commands")).unwrap_or_default()
    }
}
impl Drop for Directory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
type Handler = Arc<dyn Fn(&HttpRequest, usize) -> (u16, Value) + Send + Sync>;
pub struct Client {
    pub calls: Mutex<Vec<HttpRequest>>,
    pub handler: Handler,
    pub delay: Duration,
}
pub fn user_info(request: &HttpRequest) -> Option<(u16, Value)> {
    request
        .url
        .contains("openidconnect.googleapis.com")
        .then(|| {
            let email = request
                .headers
                .get("authorization")
                .and_then(|token| token.strip_prefix("Bearer owner."))
                .and_then(|token| token.split('~').next())
                .unwrap_or("person@example.invalid");
            (200, json!({"email":email}))
        })
}
impl Client {
    pub fn new(
        handler: impl Fn(&HttpRequest, usize) -> (u16, Value) + Send + Sync + 'static,
    ) -> Self {
        Self {
            calls: Mutex::default(),
            handler: Arc::new(handler),
            delay: Duration::ZERO,
        }
    }
}
#[async_trait]
impl HttpClient for Client {
    async fn stream(&self, request: HttpRequest) -> Result<HttpStreamingResponse, HttpClientError> {
        let (status, value) = {
            let mut calls = self.calls.lock().unwrap();
            let index = calls.len();
            calls.push(request.clone());
            (self.handler)(&request, index)
        };
        if !self.delay.is_zero() {
            tokio::time::sleep(self.delay).await;
        }
        if status == 0 {
            return Err(HttpClientError::Transport("fixture-network-failure".into()));
        }
        Ok(HttpStreamingResponse {
            status,
            headers: BTreeMap::new(),
            body: Box::pin(futures::stream::iter([Ok(serde_json::to_vec(&value)
                .unwrap()
                .into())])),
        })
    }
}
