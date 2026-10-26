use crate::*;
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Site {
    International,
    China,
    QwenWork,
    QwenWorkEnterprise,
}
#[derive(Clone, Debug)]
pub(crate) struct Deployment {
    pub web: String,
    pub openapi: String,
    pub api: String,
    pub vpc: Option<String>,
}
impl Site {
    pub fn id(self) -> &'static str {
        match self {
            Self::International => "qoder",
            Self::China => "qoder-cn",
            Self::QwenWork => "qwenwork",
            Self::QwenWorkEnterprise => "qwenwork-enterprise",
        }
    }
    pub(crate) fn client_id(self) -> &'static str {
        match self {
            Self::International => "732aef47-9cf2-46a2-95fe-4cebb5d0d1fa",
            Self::China | Self::QwenWork | Self::QwenWorkEnterprise => {
                "e883ade2-e6e3-4d6d-adf7-f92ceff5fdcb"
            }
        }
    }
    pub(crate) fn is_work(self) -> bool {
        matches!(self, Self::QwenWork | Self::QwenWorkEnterprise)
    }
    pub(crate) fn version(self) -> &'static str {
        if self.is_work() {
            "1.1.59"
        } else {
            VERSION
        }
    }
    pub(crate) fn client_type(self) -> &'static str {
        if self.is_work() {
            "6"
        } else {
            "10"
        }
    }
    pub(crate) fn product(self) -> &'static str {
        if self.is_work() {
            "qoder_work"
        } else {
            "app"
        }
    }
    pub(crate) fn scene(self) -> &'static str {
        if self.is_work() {
            "qwork"
        } else {
            "app"
        }
    }
    pub(crate) fn user_info(self) -> &'static str {
        if self.is_work() {
            "/api/v1/adapter/user/account-context?include=user,plan,quota,page,data_sharing"
        } else {
            "/api/v1/userinfo"
        }
    }
    pub(crate) fn system_prompt(self) -> &'static str {
        if self.is_work() {
            "You are QwenWork (qw), a helpful assistant. Use the instructions below and the tools available to you to assist the user."
        } else {
            "You are a Qoder agent. Use the instructions below and the tools available to you to assist the user."
        }
    }
    pub(crate) fn deployment(self, vpc: Option<&str>) -> PluginResult<Deployment> {
        if self.is_work() {
            let domain = match vpc.filter(|value| !value.is_empty()) {
                Some(value) if self == Self::QwenWorkEnterprise => enterprise_domain(value)
                    .ok_or_else(|| {
                        authentication("Enter the original QwenWork enterprise domain.")
                    })?,
                Some(_) => {
                    return Err(authentication(
                        "This public QwenWork account cannot use an enterprise domain.",
                    ))
                }
                None => "gateway.qwenwork.cn".into(),
            };
            let host = format!("https://{domain}");
            return Ok(Deployment {
                web: host.clone(),
                openapi: host.clone(),
                api: host,
                vpc: vpc.filter(|value| !value.is_empty()).map(|_| domain),
            });
        }
        let public = match self {
            Self::International => Deployment {
                web: "https://qoder.com".into(),
                openapi: "https://openapi.qoder.sh".into(),
                api: "https://api3.qoder.sh".into(),
                vpc: None,
            },
            Self::China => Deployment {
                web: "https://qoder.cn".into(),
                openapi: "https://openapi.qoder.com.cn".into(),
                api: "https://gateway.qoder.com.cn".into(),
                vpc: None,
            },
            Self::QwenWork | Self::QwenWorkEnterprise => unreachable!(),
        };
        let Some(instance) = vpc.filter(|vpc| !vpc.is_empty()) else {
            return Ok(public);
        };
        if self != Self::China {
            return Err(authentication(
                "This Qoder account has an invalid enterprise instance. Sign in again.",
            ));
        }
        let name=instance_name(instance).ok_or_else(||authentication("The Qoder enterprise instance is invalid. Sign in again with its original instance."))?;
        Ok(Deployment {
            web: format!("https://{name}.vpc.qoder.com.cn"),
            openapi: format!("https://{name}-openapi.vpc.qoder.com.cn"),
            api: format!("https://{name}-gateway.vpc.qoder.com.cn"),
            vpc: Some(name),
        })
    }
}
pub(crate) fn instance_name(input: &str) -> Option<String> {
    let mut value = input.trim().to_ascii_lowercase();
    if value.contains("://") {
        let url = url::Url::parse(&value).ok()?;
        if url.scheme() != "https"
            || !url.username().is_empty()
            || url.password().is_some()
            || url.port().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
            || url.path() != "/"
        {
            return None;
        }
        let host = url.host_str()?;
        if value.trim_end_matches('/') != format!("https://{host}") {
            return None;
        }
        value = host.into();
    }
    if value.parse::<std::net::IpAddr>().is_ok() {
        return None;
    }
    let name = if let Some(name) = value.strip_suffix(".vpc.qoder.com.cn") {
        name.strip_suffix("-openapi")
            .or_else(|| name.strip_suffix("-gateway"))
            .unwrap_or(name)
            .to_owned()
    } else {
        if value.contains('.') {
            return None;
        }
        value
    };
    if name.is_empty()
        || name.len() > 55
        || !name.as_bytes().first()?.is_ascii_alphanumeric()
        || !name.as_bytes().last()?.is_ascii_alphanumeric()
        || !name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
    {
        return None;
    }
    Some(name)
}
#[cfg(test)]
#[path = "../test/site.test.rs"]
mod tests;
pub(crate) fn encode_path(value: &str) -> String {
    value
        .as_bytes()
        .iter()
        .map(|byte| {
            if byte.is_ascii_alphanumeric() || matches!(*byte, b'-' | b'.' | b'_' | b'~') {
                (*byte as char).to_string()
            } else {
                format!("%{byte:02X}")
            }
        })
        .collect()
}

pub(crate) fn enterprise_domain(input: &str) -> Option<String> {
    let value = input.trim().to_ascii_lowercase();
    if value.is_empty() {
        return None;
    }
    let url = url::Url::parse(&if value.contains("://") {
        value.clone()
    } else {
        format!("https://{value}")
    })
    .ok()?;
    if url.scheme() != "https"
        || !url.username().is_empty()
        || url.password().is_some()
        || url.port().is_some()
        || url.path() != "/"
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return None;
    }
    let host = url.host_str()?;
    if host.parse::<std::net::IpAddr>().is_ok()
        || !host.contains('.')
        || ["localhost", "local", "internal"]
            .iter()
            .any(|suffix| host == *suffix || host.ends_with(&format!(".{suffix}")))
    {
        return None;
    }
    let parts = host.split('.').collect::<Vec<_>>();
    if parts.last()?.len() < 2
        || !parts.last()?.bytes().all(|b| b.is_ascii_alphabetic())
        || parts.iter().any(|part| {
            part.is_empty()
                || part.len() > 63
                || !part
                    .as_bytes()
                    .first()
                    .is_some_and(u8::is_ascii_alphanumeric)
                || !part
                    .as_bytes()
                    .last()
                    .is_some_and(u8::is_ascii_alphanumeric)
                || !part.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
        })
    {
        return None;
    }
    Some(host.into())
}
