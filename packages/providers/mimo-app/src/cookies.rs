use nora_provider_plugin_sdk::{PluginError, PluginResult};
use nora_provider_subscription_core as core;
use std::collections::BTreeMap;
#[derive(Clone, Debug)]
struct Entry {
    name: String,
    value: String,
    domain: String,
    host_only: bool,
    path: String,
    secure: bool,
    expires: Option<u64>,
}
#[derive(Default)]
pub(crate) struct Jar {
    cookies: Vec<Entry>,
}
fn safe(value: &str) -> bool {
    value.len() <= 64 * 1024 && !value.chars().any(char::is_control) && !value.contains(';')
}
impl Jar {
    fn put(&mut self, entry: Entry) -> PluginResult<()> {
        self.cookies.retain(|old| {
            old.expires.is_none_or(|at| at > core::now())
                && !(old.name == entry.name && old.domain == entry.domain && old.path == entry.path)
        });
        if entry.expires.is_some_and(|at| at <= core::now()) {
            return Ok(());
        }
        if self.cookies.len() >= 512 {
            return Err(PluginError::transport(
                "Xiaomi's sign-in returned too many cookies.",
            ));
        }
        self.cookies.push(entry);
        Ok(())
    }
    pub(crate) fn seed(&mut self, url: &str, name: &str, value: &str) -> PluginResult<()> {
        if !safe(name) || name.contains('=') || !safe(value) {
            return Err(PluginError::authentication(
                "Xiaomi's account cookies are invalid.",
            ));
        }
        let url = url::Url::parse(url)
            .map_err(|_| PluginError::invalid("Invalid Xiaomi cookie scope."))?;
        self.put(Entry {
            name: name.into(),
            value: value.into(),
            domain: url.host_str().unwrap_or("").into(),
            host_only: true,
            path: "/".into(),
            secure: true,
            expires: None,
        })
    }
    pub(crate) fn take(&mut self, url: &str, lines: &[String]) -> PluginResult<()> {
        let url = url::Url::parse(url)
            .map_err(|_| PluginError::invalid("Invalid Xiaomi cookie scope."))?;
        let host = url.host_str().unwrap_or("").to_lowercase();
        for line in lines {
            let Ok(cookie) = cookie::Cookie::parse(line.to_owned()) else {
                continue;
            };
            if cookie.name().is_empty()
                || !safe(cookie.name())
                || cookie.name().contains('=')
                || !safe(cookie.value())
            {
                continue;
            }
            let domain = cookie
                .domain()
                .map(|domain| domain.trim_start_matches('.').to_lowercase());
            let (domain, host_only) = if let Some(domain) = domain {
                if !(host == domain || host.ends_with(&format!(".{domain}")))
                    || !(["xiaomi.com", "xiaomimimo.com"].contains(&domain.as_str())
                        || domain.ends_with(".xiaomi.com")
                        || domain.ends_with(".xiaomimimo.com"))
                {
                    continue;
                }
                (domain, false)
            } else {
                (host.clone(), true)
            };
            let default_path = url
                .path()
                .rfind('/')
                .filter(|index| *index > 0)
                .map(|index| &url.path()[..index])
                .unwrap_or("/");
            let path = cookie
                .path()
                .filter(|path| path.starts_with('/'))
                .unwrap_or(default_path)
                .to_owned();
            let expires = if let Some(age) = cookie.max_age() {
                let seconds = age.whole_seconds();
                Some(if seconds <= 0 {
                    0
                } else {
                    core::now().saturating_add((seconds as u64).saturating_mul(1000))
                })
            } else {
                cookie.expires_datetime().map(|date| {
                    u64::try_from(date.unix_timestamp())
                        .unwrap_or(0)
                        .saturating_mul(1000)
                })
            };
            self.put(Entry {
                name: cookie.name().into(),
                value: cookie.value().into(),
                domain,
                host_only,
                path,
                secure: cookie.secure().unwrap_or(false),
                expires,
            })?;
        }
        Ok(())
    }
    fn selected(&self, url: &str) -> PluginResult<Vec<&Entry>> {
        let url = url::Url::parse(url)
            .map_err(|_| PluginError::invalid("Invalid Xiaomi cookie scope."))?;
        let host = url.host_str().unwrap_or("").to_lowercase();
        let mut values = self
            .cookies
            .iter()
            .filter(|cookie| {
                (!cookie.secure || url.scheme() == "https")
                    && cookie.expires.is_none_or(|at| at > core::now())
                    && (if cookie.host_only {
                        host == cookie.domain
                    } else {
                        host == cookie.domain || host.ends_with(&format!(".{}", cookie.domain))
                    })
                    && (cookie.path == "/"
                        || url.path() == cookie.path
                        || url.path().starts_with(&format!(
                            "{}{slash}",
                            cookie.path,
                            slash = if cookie.path.ends_with('/') { "" } else { "/" }
                        )))
            })
            .collect::<Vec<_>>();
        values.sort_by_key(|cookie| std::cmp::Reverse(cookie.path.len()));
        Ok(values)
    }
    pub(crate) fn header(&self, url: &str) -> PluginResult<String> {
        Ok(self
            .selected(url)?
            .iter()
            .map(|cookie| format!("{}={}", cookie.name, cookie.value))
            .collect::<Vec<_>>()
            .join("; "))
    }
    pub(crate) fn map(&self, url: &str) -> PluginResult<BTreeMap<String, String>> {
        let mut cookies = BTreeMap::new();
        for cookie in self.selected(url)? {
            cookies
                .entry(cookie.name.clone())
                .or_insert_with(|| cookie.value.clone());
        }
        Ok(cookies)
    }
}
pub(crate) fn header(cookies: &BTreeMap<String, String>) -> PluginResult<String> {
    let mut parts = Vec::new();
    for name in ["serviceToken", "userId", "cUserId"] {
        if let Some(value) = cookies.get(name) {
            if !safe(value) {
                return Err(PluginError::authentication(
                    "Xiaomi's session cookie is invalid.",
                ));
            }
            parts.push(format!("{name}={value}"));
        }
    }
    for (name, value) in cookies {
        if ["serviceToken", "userId", "cUserId"].contains(&name.as_str()) {
            continue;
        }
        if !safe(name) || name.contains('=') || !safe(value) {
            return Err(PluginError::authentication(
                "Xiaomi's session cookies are invalid.",
            ));
        }
        parts.push(format!("{name}={value}"));
    }
    Ok(parts.join("; "))
}
#[cfg(test)]
#[path = "../test/cookies.test.rs"]
mod tests;
