use crate::*;

pub(super) async fn run(
    provider: &WorkBuddy,
    client: &dyn HttpClient,
    credentials: &Value,
    status_only: bool,
) -> PluginResult<Value> {
    if provider.site != Site::China {
        return Ok(json!({"outcome":"inactive"}));
    }
    if credentials["type"] != "oauth" || text(credentials, "uid").is_empty() {
        return Err(signed_out());
    }
    let credentials = auth::current(provider, client, credentials).await?;
    let headers = request::headers(provider.site, &credentials);
    let status = match provider
        .call(
            client,
            HttpMethod::Post,
            "/v2/billing/meter/checkin-activity-status",
            headers.clone(),
            Some(json!({})),
        )
        .await
    {
        Ok(value) => value,
        Err(error) => return refused(provider, client, &headers, error, true).await,
    };
    let active = status["active"]
        .as_bool()
        .ok_or_else(|| PluginError::transport("WorkBuddy returned no valid check-in status."))?;
    if !active {
        return Ok(json!({"outcome":"inactive"}));
    }
    let checked = status["today_checked_in"]
        .as_bool()
        .ok_or_else(|| PluginError::transport("WorkBuddy returned no valid check-in status."))?;
    if checked {
        return Ok(result(
            "done",
            number(&status["today_credit"]),
            number(&status["streak_days"]),
        ));
    }
    if status_only {
        return Ok(json!({"outcome":"ready","retrySafe":true}));
    }
    let claim = match provider
        .call(
            client,
            HttpMethod::Post,
            "/v2/billing/meter/daily-checkin",
            headers.clone(),
            Some(json!({})),
        )
        .await
    {
        Ok(value) => value,
        Err(error) => return refused(provider, client, &headers, error, false).await,
    };
    // The vendor acknowledges a claim through its amount, streak or delayed
    // credit marker. An empty success envelope cannot confirm an effect.
    if !claim.is_object()
        || number(&claim["credit"]).is_none()
            && number(&claim["streak_days"]).is_none()
            && claim["credit_delayed"].as_bool().is_none()
    {
        return Err(PluginError::transport(
            "WorkBuddy did not confirm the daily credit claim.",
        ));
    }
    Ok(result(
        "claimed",
        number(&claim["credit"]),
        number(&claim["streak_days"]),
    ))
}
fn result(outcome: &str, credit: Option<f64>, streak: Option<f64>) -> Value {
    let mut result = json!({"outcome":outcome});
    if let Some(credit) = credit.filter(|v| *v >= 0.0) {
        result["credit"] = json!(credit);
    }
    if let Some(streak) = streak.filter(|v| *v >= 0.0 && *v <= 1_000_000.0 && v.fract() == 0.0) {
        result["streak"] = json!(streak as u64);
    }
    result
}
async fn refused(
    provider: &WorkBuddy,
    client: &dyn HttpClient,
    headers: &BTreeMap<String, String>,
    error: PluginError,
    before_claim: bool,
) -> PluginResult<Value> {
    match error.code.as_str() {
        "workbuddy_1001" => return Ok(json!({"outcome":"done"})),
        "workbuddy_1002" => return Ok(json!({"outcome":"ineligible"})),
        "workbuddy_1003" => return Ok(json!({"outcome":"inactive"})),
        "authentication_required" | "workbuddy_401" => {
            let valid = provider
                .call(
                    client,
                    HttpMethod::Post,
                    "/billing/meter/get-user-resource-summary",
                    headers.clone(),
                    Some(json!({})),
                )
                .await
                .is_ok();
            return if valid {
                Ok(json!({"outcome":"own-app"}))
            } else {
                Ok(json!({"outcome":"failed","retrySafe":true}))
            };
        }
        _ => {}
    }
    if before_claim {
        return Ok(
            json!({"outcome":"failed","retrySafe":true,"offline":matches!(error.message.as_str(),"The WorkBuddy connection stopped."|"The WorkBuddy request timed out.")}),
        );
    }
    Err(PluginError::transport(
        "The WorkBuddy claim result could not be verified. Read its status before trying again.",
    ))
}

#[cfg(test)]
#[path = "../test/checkin.test.rs"]
mod tests;
