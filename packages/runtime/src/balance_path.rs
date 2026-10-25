mod expression;
mod paths;

use serde::Serialize;
use serde_json::Value;

#[derive(Debug, Serialize)]
pub(crate) struct BalancePart {
    #[serde(skip_serializing_if = "String::is_empty")]
    pub label: String,
    pub text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub percent: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(skip)]
    pub amount: Option<f64>,
    #[serde(skip)]
    pub currency: Option<&'static str>,
}

pub(crate) fn read(reply: &Value, path: &str) -> Result<Vec<BalancePart>, String> {
    if path.len() > 16_384 || path.split(';').count() > 100 {
        return Err("The balance path has too many expressions.".into());
    }
    if !path.contains([';', ':']) {
        return one(reply, path).map(|part| vec![part]);
    }
    let mut parts = Vec::new();
    let mut succeeded = false;
    let mut first_error = None;
    for part in path
        .split(';')
        .map(str::trim)
        .filter(|part| !part.is_empty())
    {
        let (label, path) = part
            .split_once(':')
            .map(|(label, path)| (label.trim(), path))
            .unwrap_or(("", part));
        match one(reply, path) {
            Ok(mut part) => {
                succeeded = true;
                part.label = label.into();
                parts.push(part);
            }
            Err(error) => {
                let error = if label.is_empty() {
                    error
                } else {
                    format!("{label}: {error}")
                };
                first_error.get_or_insert_with(|| error.clone());
                parts.push(BalancePart {
                    label: label.into(),
                    text: "—".into(),
                    percent: None,
                    error: Some(error),
                    amount: None,
                    currency: None,
                });
            }
        }
    }
    if !succeeded {
        return Err(first_error.unwrap_or_else(|| {
            "Choose where in the balance response its amount is, for example data.balance.".into()
        }));
    }
    Ok(parts)
}

fn one(reply: &Value, path: &str) -> Result<BalancePart, String> {
    let mut path = path.trim();
    let mut sign = "";
    let mut currency = None;
    for (symbol, code) in [("$", "USD"), ("¥", "CNY"), ("€", "EUR"), ("£", "GBP")] {
        if let Some(rest) = path.strip_prefix(symbol) {
            path = rest.trim();
            sign = symbol;
            currency = Some(code);
            break;
        }
    }
    let percent = path.ends_with('%');
    if percent {
        path = path.strip_suffix('%').unwrap().trim();
    }
    if path.is_empty() {
        return Err(
            "Choose where in the balance response its amount is, for example data.balance.".into(),
        );
    }
    let lone = !path.contains(['+', '-', '*', '/', '(', ')', ' ', '['])
        && !path.as_bytes()[0].is_ascii_digit();
    let number = if lone {
        let (values, many) = paths::resolve(reply, path)?;
        if many {
            return Err(
                "The balance path has several values; put them in sum(), max(), min() or count()."
                    .into(),
            );
        }
        let value = values
            .first()
            .copied()
            .flatten()
            .ok_or("The balance path is missing from the response.")?;
        match number(value) {
            Some(number) => number,
            None => {
                if let Some(value) = value.as_str().filter(|value| !value.is_empty() && !percent) {
                    return Ok(BalancePart {
                        label: String::new(),
                        text: format!("{sign}{value}"),
                        percent: None,
                        error: None,
                        amount: None,
                        currency,
                    });
                }
                return Err("The balance path does not contain an amount.".into());
            }
        }
    } else {
        expression::evaluate(reply, path)?
    };
    if !number.is_finite() {
        return Err("The balance path does not produce a finite amount.".into());
    }
    let share = percent.then_some(number * 100.0);
    if share.is_some_and(|share| !share.is_finite()) {
        return Err("The balance percentage is outside the supported range.".into());
    }
    let text = match share {
        Some(share) => {
            let value = format!("{share:.1}");
            format!("{sign}{}%", value.strip_suffix(".0").unwrap_or(&value))
        }
        None => format!("{sign}{number:.2}"),
    };
    Ok(BalancePart {
        label: String::new(),
        text,
        percent: share,
        error: None,
        amount: Some(number),
        currency,
    })
}

fn number(value: &Value) -> Option<f64> {
    value
        .as_f64()
        .or_else(|| value.as_str()?.trim().parse().ok())
        .filter(|number| number.is_finite())
}

#[cfg(test)]
#[path = "../test/balance_path.test.rs"]
mod tests;
