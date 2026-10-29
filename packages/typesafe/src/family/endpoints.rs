use super::*;

pub(super) fn url(base: &str) -> PluginResult<Url> {
    let value =
        Url::parse(base).map_err(|_| PluginError::invalid("Enter a valid decision endpoint."))?;
    let local = value
        .host_str()
        .is_some_and(|host| matches!(host, "localhost" | "127.0.0.1" | "[::1]" | "::1"));
    if base.len() > 8192
        || !(value.scheme() == "https" || value.scheme() == "http" && local)
        || value.host_str().is_none()
        || !value.username().is_empty()
        || value.password().is_some()
        || value.query().is_some()
        || value.fragment().is_some()
    {
        return Err(PluginError::invalid(
            "A decision endpoint requires HTTPS without credentials, query or fragment.",
        ));
    }
    Ok(value)
}

pub(super) fn append(base: &str, path: &str) -> PluginResult<String> {
    let mut value = url(base)?;
    value.set_path(&format!("{}/{}", value.path().trim_end_matches('/'), path));
    Ok(value.into())
}

pub(super) fn normalized(family: DecisionFamily, base: &str) -> PluginResult<String> {
    let mut value = url(base)?;
    let mut path = value.path().trim_end_matches('/').to_owned();
    for suffix in ["/systemone", "/models"] {
        path = path.trim_end_matches(suffix).to_owned();
    }
    if family == DecisionFamily::Vercel {
        if !path.ends_with("/v4/ai") {
            path = path.trim_end_matches("/v1").to_owned();
            if path.is_empty() {
                path = "/typesafe".into();
            }
            if !path.ends_with("/typesafe") {
                return Err(PluginError::invalid(
                    "Use Vercel's TypeSafe endpoint, or its existing /v4/ai endpoint.",
                ));
            }
            path.push_str("/v1");
        }
    }
    value.set_path(&path);
    Ok(value.to_string().trim_end_matches('/').into())
}

pub(super) fn legacy(base: &str) -> bool {
    base.ends_with("/v4/ai")
}

pub(super) fn vercel_root(base: &str) -> PluginResult<String> {
    let mut value = url(base)?;
    let path = value.path().trim_end_matches('/');
    let root = path
        .strip_suffix("/typesafe/v1")
        .or_else(|| path.strip_suffix("/v4/ai"))
        .ok_or_else(|| PluginError::invalid("Invalid Vercel decision endpoint."))?;
    let root = root.to_owned();
    value.set_path(&root);
    Ok(value.to_string().trim_end_matches('/').into())
}

pub(super) fn component(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
}

pub(super) fn cloudflare(base: &str) -> PluginResult<(String, Option<String>)> {
    let mut value = url(base)?;
    let path = value.path().trim_end_matches('/');
    let (api, account) = if let Some((api, rest)) = path.split_once("/accounts") {
        let account = rest.trim_start_matches('/').split('/').next().unwrap_or("");
        let lower = account.to_ascii_lowercase();
        let placeholder = account.starts_with(['$', '{', '<', ':'])
            || ["%24", "%7b", "%3c", "%3a"]
                .iter()
                .any(|prefix| lower.starts_with(prefix));
        if !account.is_empty() && !placeholder && !component(account) {
            return Err(PluginError::invalid("Enter a valid Cloudflare account ID."));
        }
        let suffix = rest
            .trim_start_matches('/')
            .strip_prefix(account)
            .unwrap_or("");
        if !matches!(suffix, "" | "/ai/run") {
            return Err(PluginError::invalid(
                "Use the Cloudflare account endpoint or its /ai/run endpoint.",
            ));
        }
        (
            api.to_owned(),
            (!account.is_empty() && !placeholder).then(|| account.to_owned()),
        )
    } else {
        (path.to_owned(), None)
    };
    if !api.ends_with("/client/v4") {
        return Err(PluginError::invalid(
            "Use Cloudflare's /client/v4 endpoint.",
        ));
    }
    value.set_path(&api);
    Ok((value.to_string().trim_end_matches('/').into(), account))
}

pub(super) fn cloudflare_run(api: &str, account: &str) -> PluginResult<String> {
    if !component(account) {
        return Err(PluginError::invalid("Enter a valid Cloudflare account ID."));
    }
    append(api, &format!("accounts/{account}/ai/run"))
}

pub(super) fn bailian(method: &str, inputs: &Value) -> PluginResult<String> {
    if method == "token-plan" {
        return Ok("https://token-plan.cn-beijing.maas.aliyuncs.com/compatible-mode/v1".into());
    }
    let region = match method {
        "apiKey" | "cn-beijing" => "cn-beijing",
        "ap-southeast-1" => "ap-southeast-1",
        _ => return Err(PluginError::unsupported("authMethodID")),
    };
    let workspace = core::required(inputs, "workspaceId")?;
    if workspace.len() > 63
        || !component(workspace)
        || workspace.contains('_')
        || workspace.starts_with('-')
        || workspace.ends_with('-')
    {
        return Err(PluginError::invalid(
            "Enter your Bailian workspace ID, without a URL.",
        ));
    }
    Ok(format!(
        "https://{workspace}.{region}.maas.aliyuncs.com/compatible-mode/v1"
    ))
}

#[cfg(test)]
#[path = "../../test/endpoints.test.rs"]
mod tests;
