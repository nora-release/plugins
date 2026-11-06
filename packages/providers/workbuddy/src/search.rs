use super::*;

pub(super) async fn search(
    provider: &WorkBuddy,
    context: CallContext,
    query: &str,
) -> PluginResult<Value> {
    let client = provider.client(context.network_proxy.as_ref())?;
    let credentials = auth::current(
        provider,
        client.as_ref(),
        context.credentials.as_ref().ok_or_else(signed_out)?,
    )
    .await?;
    let mut headers = request::headers(provider.site, &credentials);
    let mut bytes = [0u8; 16];
    getrandom::getrandom(&mut bytes)
        .map_err(|_| invalid("A WorkBuddy search request ID could not be created."))?;
    let id = bytes
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    headers.insert("x-request-id".into(), id.clone());
    headers.insert("x-conversation-message-id".into(), id);
    let response = send(
        client.as_ref(),
        HttpRequest {
            method: HttpMethod::Post,
            url: format!("{}/agenttool/v1/search", provider.site.endpoint()),
            headers,
            body: Some(
                json!({"query":query,"type":"text2text","max_results":5})
                    .to_string()
                    .into_bytes(),
            ),
        },
        Duration::from_secs(60),
    )
    .await?;
    let (status, _, body) = super::bytes(response, 4 * 1024 * 1024).await?;
    if status == 401 {
        return Err(signed_out());
    }
    if !(200..300).contains(&status) {
        return Err(PluginError::transport(format!(
            "WorkBuddy search returned HTTP {status}."
        )));
    }
    let value: Value = serde_json::from_slice(&body)
        .map_err(|_| PluginError::transport("WorkBuddy returned invalid search JSON."))?;
    let code = value
        .get("code")
        .filter(|code| !code.is_null())
        .map(|code| {
            code.as_str()
                .map(str::to_owned)
                .unwrap_or_else(|| code.to_string())
        });
    if code.as_deref().is_some_and(|code| code != "0") {
        return Err(PluginError::transport(
            "WorkBuddy refused this search. Check the account's search allowance.",
        ));
    }
    let rows = value
        .get("results")
        .and_then(Value::as_array)
        .ok_or_else(|| PluginError::transport("WorkBuddy returned no search results."))?;
    let rows = rows
        .iter()
        .map(|row| {
            let mut row = row.clone();
            if text(&row, "title").trim().is_empty() {
                row["title"] = json!(text(&row, "site"));
            }
            row
        })
        .collect::<Vec<_>>();
    nora_provider_search_api::provider_results(&rows)
}

#[cfg(test)]
#[path = "../test/search.test.rs"]
mod tests;
