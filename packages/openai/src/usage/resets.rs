use nora_provider::usage::parsing::{number, timestamp};
use serde_json::{json, Value};

pub fn unavailable(message: &str) -> Value {
    json!({"available":false,"grants":[],"message":message})
}

pub fn from_usage(payload: &Value) -> Value {
    parse(&payload["rate_limit_reset_credits"])
}

pub fn parse(payload: &Value) -> Value {
    let count = available_count(payload);
    let records = records(payload, 0);
    if count.is_none() && records.is_none() {
        return unavailable("Reset credit totals were not provided.");
    }
    let mut result = json!({"available":true,"grants":[]});
    if let Some(count) = count {
        result["totalAvailable"] = json!(count);
    } else if records.as_ref().is_some_and(|records| records.is_empty()) {
        result["totalAvailable"] = json!(0);
    }
    if let Some(records) = records {
        result["grants"] = Value::Array(records.into_iter().map(grant).collect());
    }
    result
}

pub fn available_count(payload: &Value) -> Option<u64> {
    for value in [
        payload,
        &payload["data"],
        &payload["reset_credits"],
        &payload["resetCredits"],
        &payload["rate_limit_reset_credits"],
        &payload["rateLimitResetCredits"],
    ] {
        for key in [
            "available_count",
            "availableCount",
            "total_available",
            "totalAvailable",
        ] {
            if let Some(number) = number(&value[key])
                .filter(|value| *value >= 0.0 && value.fract() == 0.0 && *value <= u64::MAX as f64)
            {
                return Some(number as u64);
            }
        }
    }
    None
}

fn records(payload: &Value, depth: usize) -> Option<Vec<&Value>> {
    if depth > 3 {
        return None;
    }
    if let Some(records) = payload.as_array() {
        if records.is_empty() {
            return Some(Vec::new());
        }
        let records = records
            .iter()
            .filter(|value| looks_like_grant(value))
            .collect::<Vec<_>>();
        return (!records.is_empty()).then_some(records);
    }
    if !payload.is_object() {
        return None;
    }
    for key in [
        "credits",
        "reset_credits",
        "resetCredits",
        "rate_limit_reset_credits",
        "rateLimitResetCredits",
        "items",
        "data",
        "results",
    ] {
        if let Some(records) = payload.get(key).and_then(|value| records(value, depth + 1)) {
            return Some(records);
        }
    }
    looks_like_grant(payload).then_some(vec![payload])
}

fn looks_like_grant(value: &Value) -> bool {
    value.is_object()
        && [
            "status",
            "state",
            "credit_status",
            "creditStatus",
            "granted_at",
            "grantedAt",
            "issued_at",
            "issuedAt",
            "expires_at",
            "expiresAt",
            "redeemed_at",
            "redeemedAt",
            "redeem_started_at",
            "redeemStartedAt",
            "redeemed",
            "is_redeemed",
            "isRedeemed",
        ]
        .iter()
        .any(|key| value.get(key).is_some())
}

fn grant(value: &Value) -> Value {
    let mut result = json!({"status":status(value)});
    if let Some(id) = first(value, &["id", "credit_id", "creditId"]).and_then(Value::as_str) {
        result["id"] = json!(id);
    }
    for (target, keys) in [
        (
            "grantedAt",
            &[
                "granted_at",
                "grantedAt",
                "grant_date",
                "grantDate",
                "issued_at",
                "issuedAt",
                "issued_date",
                "issuedDate",
                "created_at",
                "createdAt",
            ][..],
        ),
        (
            "expiresAt",
            &[
                "expires_at",
                "expiresAt",
                "expiry_date",
                "expiryDate",
                "expiration_date",
                "expirationDate",
                "valid_until",
                "validUntil",
            ][..],
        ),
        ("redeemedAt", &["redeemed_at", "redeemedAt"][..]),
    ] {
        if let Some(date) = first(value, keys).and_then(timestamp) {
            result[target] = json!(date);
        }
    }
    result
}

fn status(value: &Value) -> String {
    if let Some(status) = first(
        value,
        &[
            "status",
            "state",
            "credit_status",
            "creditStatus",
            "redemption_status",
            "redemptionStatus",
            "redeem_status",
            "redeemStatus",
            "redemption_state",
            "redemptionState",
        ],
    )
    .and_then(Value::as_str)
    {
        return status.trim().to_lowercase();
    }
    if let Some(redeemed) =
        first(value, &["redeemed", "is_redeemed", "isRedeemed"]).and_then(Value::as_bool)
    {
        return if redeemed { "redeemed" } else { "not_redeemed" }.into();
    }
    if first(value, &["redeemed_at", "redeemedAt"]).is_some() {
        return "redeemed".into();
    }
    if first(value, &["redeem_started_at", "redeemStartedAt"]).is_some() {
        return "redeeming".into();
    }
    "unknown".into()
}

fn first<'a>(value: &'a Value, keys: &[&str]) -> Option<&'a Value> {
    keys.iter().find_map(|key| {
        value
            .get(key)
            .filter(|value| !value.is_null() && value.as_str() != Some(""))
    })
}
