//! OAuth's device authorization grant (RFC 8628): a code the user types on
//! the vendor's page, then the token endpoint asked, no more often than it
//! allows, until they have.

use crate::{
    collect_body_text_limited, HttpClient, HttpMethod, HttpRequest, PluginError, PluginResult,
};
use serde_json::Value;
use std::time::{Duration, Instant};

/// A vendor's device sign-in: its endpoints, its client and what it asks for.
#[derive(Clone, Debug)]
pub struct DeviceFlow {
    /// Who signs in, in the vendor's words ("GitHub", "xAI").
    pub vendor: &'static str,
    pub device_code_url: &'static str,
    pub token_url: &'static str,
    pub client_id: &'static str,
    pub scope: &'static str,
    /// The page to type the code at when the vendor names none.
    pub verification_url: &'static str,
    /// Headers every call carries, such as the client the vendor expects.
    pub headers: Vec<(&'static str, String)>,
}

/// A device sign-in waiting on its user: the code the vendor gave, asked
/// about no more often than it allows.
#[derive(Debug)]
pub struct DevicePending {
    pub device_code: String,
    pub interval: Duration,
    pub next: Instant,
    pub expires: Instant,
}

/// What the device flow's start answered: the page to open, the code to
/// type there and the sign-in to wait on.
#[derive(Debug)]
pub struct DeviceStarted {
    pub url: String,
    pub code: String,
    pub pending: DevicePending,
}

impl DeviceFlow {
    fn request(&self, url: &str, pairs: &[(&str, &str)]) -> HttpRequest {
        let mut form = url::form_urlencoded::Serializer::new(String::new());
        for (key, value) in pairs {
            form.append_pair(key, value);
        }
        let mut request = HttpRequest::new(HttpMethod::Post, url)
            .with_header("accept", "application/json")
            .with_header("content-type", "application/x-www-form-urlencoded")
            .with_body(form.finish().into_bytes());
        for (name, value) in &self.headers {
            request = request.with_header(*name, value.clone());
        }
        request
    }

    async fn json(
        &self,
        client: &dyn HttpClient,
        request: HttpRequest,
    ) -> PluginResult<(u16, Value)> {
        let vendor = self.vendor;
        let response = tokio::time::timeout(Duration::from_secs(30), client.stream(request))
            .await
            .map_err(|_| PluginError::transport(format!("{vendor} took too long to answer.")))?
            .map_err(|error| {
                PluginError::transport(format!("{vendor} could not be reached: {error}"))
            })?;
        let status = response.status;
        let body = collect_body_text_limited(response.body, 1024 * 1024)
            .await
            .map_err(|_| PluginError::transport(format!("{vendor}'s answer was cut short.")))?;
        Ok((status, serde_json::from_str(&body).unwrap_or(Value::Null)))
    }

    /// Asks for a code for the user to type.
    pub async fn start(&self, client: &dyn HttpClient) -> PluginResult<DeviceStarted> {
        let request = self.request(
            self.device_code_url,
            &[("client_id", self.client_id), ("scope", self.scope)],
        );
        let (status, value) = self.json(client, request).await?;
        let text = |key: &str| value[key].as_str().unwrap_or_default().to_owned();
        // the page with the code filled in, when the vendor gives one
        let url = [text("verification_uri_complete"), text("verification_uri")]
            .into_iter()
            .find(|url| !url.is_empty())
            .unwrap_or_default();
        let (device_code, code) = (text("device_code"), text("user_code"));
        if !(200..300).contains(&status) || device_code.is_empty() || code.is_empty() {
            return Err(PluginError::transport(explain(
                &format!("{} did not start a sign-in", self.vendor),
                status,
                &value,
            )));
        }
        let now = Instant::now();
        let interval = Duration::from_secs(value["interval"].as_u64().unwrap_or(5).max(1));
        let lifetime = Duration::from_secs(value["expires_in"].as_u64().unwrap_or(900));
        Ok(DeviceStarted {
            url: if url.is_empty() {
                self.verification_url.into()
            } else {
                url
            },
            code,
            pending: DevicePending {
                device_code,
                interval,
                next: now + interval,
                expires: now + lifetime,
            },
        })
    }

    /// Asks the token endpoint once whether the user typed the code: its
    /// tokens once they did, none while they have not.
    pub async fn poll(
        &self,
        client: &dyn HttpClient,
        pending: &mut DevicePending,
    ) -> PluginResult<Option<Value>> {
        let expired = || {
            PluginError::authentication(format!(
                "The {} sign-in code expired. Sign in again.",
                self.vendor
            ))
        };
        let now = Instant::now();
        if now >= pending.expires {
            return Err(expired());
        }
        if now < pending.next {
            return Ok(None);
        }
        let request = self.request(
            self.token_url,
            &[
                ("client_id", self.client_id),
                ("device_code", &pending.device_code),
                ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
            ],
        );
        let (status, value) = self.json(client, request).await?;
        if value["access_token"]
            .as_str()
            .is_some_and(|token| !token.is_empty())
        {
            return Ok(Some(value));
        }
        match value["error"].as_str().unwrap_or_default() {
            "authorization_pending" => {}
            "slow_down" => {
                let asked = value["interval"].as_u64().map(Duration::from_secs);
                pending.interval = asked.unwrap_or(pending.interval + Duration::from_secs(5));
            }
            "expired_token" => return Err(expired()),
            "access_denied" => {
                return Err(PluginError::authentication(format!(
                    "The {} sign-in was cancelled.",
                    self.vendor
                )))
            }
            _ => {
                return Err(PluginError::authentication(explain(
                    &format!("{} refused the sign-in", self.vendor),
                    status,
                    &value,
                )))
            }
        }
        pending.next = Instant::now() + pending.interval;
        Ok(None)
    }
}

/// What a vendor said went wrong, after what was being done.
pub fn explain(doing: &str, status: u16, value: &Value) -> String {
    let message = ["error_description", "message", "error"]
        .iter()
        .find_map(|key| value[*key].as_str().filter(|text| !text.is_empty()))
        .or_else(|| value.pointer("/error/message").and_then(Value::as_str));
    match message {
        Some(message) => format!("{doing} (HTTP {status}): {message}"),
        None => format!("{doing} (HTTP {status})."),
    }
}

#[cfg(test)]
#[path = "../test/device_flow.test.rs"]
mod tests;
