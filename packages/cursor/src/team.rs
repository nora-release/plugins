use crate::network::Client;
use async_trait::async_trait;
use base64::Engine;
use nora_ai::{HttpClient, HttpClientError, HttpRequest, HttpStreamingResponse};
use serde_json::Value;
use std::{
    collections::BTreeMap,
    io::Read,
    path::PathBuf,
    sync::{Arc, Mutex},
    time::Duration,
};

#[derive(Default)]
struct Cached {
    path: Option<PathBuf>,
    at: Option<tokio::time::Instant>,
    info: Option<Value>,
}

#[derive(Default)]
pub(crate) struct Teams {
    cached: Mutex<Cached>,
    #[cfg(test)]
    path: Option<PathBuf>,
}

fn config_path(environment: &BTreeMap<String, String>) -> Option<PathBuf> {
    let directory = |key: &str| {
        environment
            .get(key)
            .map(|value| value.trim())
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
    };
    directory("CURSOR_CONFIG_DIR")
        .or_else(|| directory("XDG_CONFIG_HOME").map(|path| path.join("cursor")))
        .or_else(|| {
            directory("HOME")
                .or_else(|| directory("USERPROFILE"))
                .map(|path| path.join(".cursor"))
        })
        .map(|path| path.join("cli-config.json"))
}

fn subject(token: &str) -> Option<String> {
    let parts = token.split('.').collect::<Vec<_>>();
    if parts.len() != 3 || parts[1].len() > 64 * 1024 {
        return None;
    }
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(parts[1].trim_end_matches('='))
        .ok()?;
    let value: Value = serde_json::from_slice(&bytes).ok()?;
    value["sub"]
        .as_str()
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

fn team_id(value: &Value) -> Option<u64> {
    let id = value.as_f64()?;
    (id.is_finite() && id > 0.0 && id <= 9_007_199_254_740_991.0 && id.fract() == 0.0)
        .then_some(id as u64)
}

impl Teams {
    pub(crate) fn active(&self, token: &str) -> Option<u64> {
        let subject = subject(token)?;
        let path = config_path(&std::env::vars().collect());
        #[cfg(test)]
        let path = self.path.clone().or(path);
        let mut cached = self
            .cached
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if cached.path != path {
            *cached = Cached {
                path: path.clone(),
                ..Default::default()
            };
        }
        if cached
            .at
            .is_none_or(|at| at.elapsed() >= Duration::from_secs(10))
        {
            if let Some(path) = path {
                // Cursor rewrites this file in place. Keep the last parsed value
                // through a partial write, but clear it when the file is deleted.
                match std::fs::File::open(path) {
                    Ok(file) => {
                        let mut bytes = Vec::new();
                        if file.take(1024 * 1024 + 1).read_to_end(&mut bytes).is_ok()
                            && bytes.len() <= 1024 * 1024
                        {
                            if let Ok(value) = serde_json::from_slice::<Value>(&bytes) {
                                cached.info = value.get("authInfo").cloned();
                            }
                        }
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                        cached.info = None
                    }
                    Err(_) => {}
                }
            }
            cached.at = Some(tokio::time::Instant::now());
        }
        let info = cached.info.as_ref()?;
        if info["authId"].as_str()? != subject {
            return None;
        }
        team_id(&info["activeTeamId"])
    }

    pub(crate) fn client(self: &Arc<Self>, inner: Client) -> Client {
        Arc::new(TeamClient {
            inner,
            selection: Selection::Live(self.clone()),
        })
    }
}

enum Selection {
    Live(Arc<Teams>),
    Fixed {
        subject: Option<String>,
        team: Option<u64>,
    },
}

struct TeamClient {
    inner: Client,
    selection: Selection,
}

pub(crate) fn fixed_client(inner: Client, token: &str, team: Option<u64>) -> Client {
    Arc::new(TeamClient {
        inner,
        selection: Selection::Fixed {
            subject: subject(token),
            team,
        },
    })
}

#[async_trait]
impl HttpClient for TeamClient {
    async fn stream(
        &self,
        mut request: HttpRequest,
    ) -> Result<HttpStreamingResponse, HttpClientError> {
        let token = request
            .headers
            .get("authorization")
            .and_then(|value| value.strip_prefix("Bearer "));
        let team = token.and_then(|token| match &self.selection {
            Selection::Live(teams) => teams.active(token),
            Selection::Fixed {
                subject: owner,
                team,
            } => (owner.is_some() && *owner == subject(token))
                .then_some(*team)
                .flatten(),
        });
        request.headers.remove("x-cursor-team-id");
        if let Some(team) = team {
            request = request.with_header("x-cursor-team-id", team.to_string());
        }
        self.inner.stream(request).await
    }
}

#[cfg(test)]
#[path = "../test/team.test.rs"]
mod tests;
