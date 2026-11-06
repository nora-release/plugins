use crate::*;
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Site {
    China,
    Global,
}
pub(crate) const FUNCTIONS: [&str; 4] =
    ["chat_v3", "solo_work_lite", "solo_agent", "solo_agent_lite"];
pub(crate) const CLIENT: &str = "ono9krqynydwx5";
pub(crate) const BRAND: &str = "ASUS TUF Gaming A15 FA507RM_FA507RM";
impl Site {
    pub fn id(self) -> &'static str {
        match self {
            Self::China => "trae-cn",
            Self::Global => "trae-global",
        }
    }
    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::China => "Trae CN",
            Self::Global => "Trae Global",
        }
    }
    pub(crate) fn web(self) -> &'static str {
        match self {
            Self::China => "https://www.trae.cn",
            Self::Global => "https://www.trae.ai",
        }
    }
    pub(crate) fn auth(self) -> &'static str {
        match self {
            Self::China => "https://api.trae.cn",
            Self::Global => "https://growsg-normal.trae.ai",
        }
    }
    pub(crate) fn api(self, credentials: &Value) -> String {
        let host = host(text(credentials, "api")).unwrap_or_default();
        let name = url::Url::parse(&host)
            .ok()
            .and_then(|url| url.host_str().map(str::to_owned))
            .unwrap_or_default();
        match self {
            Self::China => {
                if !name.is_empty()
                    && (name == "mchost.guru"
                        || name.ends_with(".mchost.guru")
                        || name.starts_with("trae-api-")
                        || name.ends_with(".trae.cn")
                            && !["www.", "api.", "grow"]
                                .iter()
                                .any(|prefix| name.starts_with(prefix)))
                {
                    host
                } else {
                    "https://trae-api-cn.mchost.guru".into()
                }
            }
            Self::Global => {
                let region = text(credentials, "region").to_ascii_uppercase();
                if name.contains("coreva")
                    || name.starts_with("api-us-")
                    || region == "US"
                    || ["US-", "US_", "US "]
                        .iter()
                        .any(|prefix| region.starts_with(prefix))
                {
                    "https://coreva-normal.trae.ai".into()
                } else if name.contains("coresg")
                    || name.ends_with(".trae.ai")
                        && !["www.", "api-", "api.", "grow"]
                            .iter()
                            .any(|prefix| name.starts_with(prefix))
                {
                    host
                } else {
                    "https://coresg-normal.trae.ai".into()
                }
            }
        }
    }
    pub(crate) fn auth_host(self, credentials: &Value) -> String {
        host(text(credentials, "authHost"))
            .filter(|host| {
                let name = url::Url::parse(host)
                    .ok()
                    .and_then(|url| url.host_str().map(str::to_owned))
                    .unwrap_or_default();
                let domain = if self == Self::China {
                    "trae.cn"
                } else {
                    "trae.ai"
                };
                name == domain || name.ends_with(&format!(".{domain}"))
            })
            .unwrap_or_else(|| self.auth().into())
    }
    pub(crate) fn pay(self, credentials: &Value) -> &'static str {
        if self == Self::China {
            return self.auth();
        }
        let region = text(credentials, "region").to_ascii_uppercase();
        if region == "US"
            || ["US-", "US_", "US "]
                .iter()
                .any(|prefix| region.starts_with(prefix))
            || text(credentials, "api").contains("api-us")
        {
            "https://api-us-east.trae.ai"
        } else {
            "https://api-sg-central.trae.ai"
        }
    }
    pub(crate) fn plugin(self) -> &'static str {
        match self {
            Self::China => "2.3.24254",
            Self::Global => "2.3.62834",
        }
    }
    pub(crate) fn ide(self) -> &'static str {
        match self {
            Self::China => "3.3.65",
            Self::Global => "3.5.51",
        }
    }
    pub(crate) fn lapsed(self, code: &str) -> bool {
        ["1001", "401"].contains(&code) || self == Self::Global && code == "20101"
    }
    pub(crate) fn quota(self, code: &str) -> bool {
        ["4008", "1005"].contains(&code) || self == Self::Global && code == "4011"
    }
}
pub(crate) fn host(value: &str) -> Option<String> {
    let url = url::Url::parse(value.trim()).ok()?;
    if url.scheme() != "https"
        || !url.username().is_empty()
        || url.password().is_some()
        || url.port().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return None;
    }
    let name = url.host_str()?.to_ascii_lowercase();
    if !["trae.cn", "trae.ai", "mchost.guru"]
        .iter()
        .any(|domain| name == *domain || name.ends_with(&format!(".{domain}")))
    {
        return None;
    }
    Some(url.as_str().trim_end_matches('/').into())
}
pub(crate) fn ide_headers(credentials: &Value) -> BTreeMap<String, String> {
    let token = text(credentials, "access");
    BTreeMap::from([
        ("content-type".into(), "application/json".into()),
        ("authorization".into(), format!("Cloud-IDE-JWT {token}")),
        ("x-cloudide-token".into(), token.into()),
        ("x-ide-token".into(), token.into()),
        ("x-uid".into(), text(credentials, "uid").into()),
        (
            "x-app-id".into(),
            "6eefa01c-1036-4c7e-9ca5-d891f63bfcd8".into(),
        ),
        ("x-device-id".into(), text(credentials, "deviceId").into()),
        ("x-machine-id".into(), text(credentials, "machineId").into()),
        ("x-request-id".into(), uuid::Uuid::new_v4().to_string()),
        ("x-app-version".into(), "0.1.69".into()),
        ("x-app-version-code".into(), "20260917".into()),
        ("x-ide-version".into(), "0.1.69".into()),
        ("x-ide-version-code".into(), "20260917".into()),
        ("x-ide-version-type".into(), "stable".into()),
        ("x-device-cpu".into(), "AMD".into()),
        ("x-device-brand".into(), BRAND.into()),
        ("x-device-type".into(), "windows".into()),
        ("x-os-version".into(), "Windows 10".into()),
        ("x-system-type".into(), "Windows".into()),
    ])
}
#[cfg(test)]
#[path = "../test/site.test.rs"]
mod tests;
