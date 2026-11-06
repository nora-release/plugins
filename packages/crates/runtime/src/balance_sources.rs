use crate::{balance_path, definition::Definition};
use nora_provider_plugin_sdk::{PluginError, PluginResult};
use serde_json::{json, Value};

#[derive(Clone, Copy)]
pub(crate) enum Reader {
    Path,
    DeepSeek,
    OpenRouter,
    CommandCode,
    AiHubMix,
}
pub(crate) struct Source {
    pub url: String,
    pub path: String,
    pub reader: Reader,
}

pub(crate) fn known(definition: &Definition) -> Option<Source> {
    let bases = crate::discovery::configured_bases(definition, None);
    std::iter::once(definition.base_url.as_str())
        .chain(bases.iter().map(|base| base.base_url.as_str()))
        .filter_map(|raw| url::Url::parse(raw).ok())
        .filter(|url| {
            url.scheme() == "https"
                && url.port_or_known_default() == Some(443)
                && url.username().is_empty()
                && url.password().is_none()
        })
        .find_map(|url| {
            let host = url.host_str()?;
            let (path, field, reader) = match host {
                "api.deepseek.com" => ("/user/balance", "", Reader::DeepSeek),
                "api.moonshot.cn" => (
                    "/v1/users/me/balance",
                    "¥data.available_balance",
                    Reader::Path,
                ),
                "api.moonshot.ai" => (
                    "/v1/users/me/balance",
                    "$data.available_balance",
                    Reader::Path,
                ),
                "openrouter.ai" => ("/api/v1/credits", "", Reader::OpenRouter),
                "api.siliconflow.cn" => ("/v1/user/info", "¥data.totalBalance", Reader::Path),
                "api.siliconflow.com" => ("/v1/user/info", "$data.totalBalance", Reader::Path),
                "api.stepfun.com" => ("/v1/accounts", "¥balance", Reader::Path),
                "api.stepfun.ai" => ("/v1/accounts", "$balance", Reader::Path),
                "api.commandcode.ai" => ("/alpha/billing/credits", "", Reader::CommandCode),
                "ollama.com" => ("/api/balance", "$purchased.balance_usd", Reader::Path),
                "aihubmix.com" if !definition.config.balance_token.trim().is_empty() => {
                    ("/api/user/self", "$data.quota / 500000", Reader::Path)
                }
                "aihubmix.com" => ("/dashboard/billing/remain", "", Reader::AiHubMix),
                _ => return None,
            };
            Some(Source {
                url: format!("https://{host}{path}"),
                path: field.into(),
                reader,
            })
        })
}

pub(crate) fn read(source: &Source, reply: &Value) -> PluginResult<Vec<balance_path::BalancePart>> {
    let result = match source.reader {
        Reader::Path => balance_path::read(reply, &source.path),
        Reader::DeepSeek => {
            let mut balances = reply
                .get("balance_infos")
                .and_then(Value::as_array)
                .ok_or_else(missing)?
                .iter()
                .filter_map(|balance| {
                    let currency = balance["currency"].as_str()?;
                    let sign = match currency {
                        "CNY" => "¥",
                        "USD" => "$",
                        _ => return None,
                    };
                    let value = number(&balance["total_balance"])?;
                    Some((value, currency, sign))
                })
                .collect::<Vec<_>>();
            balances.sort_by(|left, right| {
                (left.0 <= 0.0, left.1 != "CNY", left.1).cmp(&(
                    right.0 <= 0.0,
                    right.1 != "CNY",
                    right.1,
                ))
            });
            if balances.is_empty() {
                return Err(missing());
            }
            let mut parts = Vec::new();
            for (value, _, sign) in balances {
                parts.extend(
                    balance_path::read(&json!({"amount":value}), &format!("{sign}amount"))
                        .map_err(PluginError::invalid)?,
                );
            }
            Ok(parts)
        }
        Reader::OpenRouter => {
            let total = number(&reply["data"]["total_credits"]).ok_or_else(missing)?;
            let used = reply["data"]
                .get("total_usage")
                .map(number)
                .unwrap_or(Some(0.0))
                .ok_or_else(missing)?;
            balance_path::read(&json!({"amount":total-used}), "$amount")
        }
        Reader::CommandCode => {
            let values = ["monthlyCredits", "purchasedCredits", "freeCredits"]
                .into_iter()
                .filter_map(|key| reply["credits"].get(key))
                .collect::<Vec<_>>();
            if values.is_empty() {
                return Err(missing());
            }
            let amount = values
                .into_iter()
                .map(|value| number(value).ok_or_else(missing))
                .collect::<PluginResult<Vec<_>>>()?
                .into_iter()
                .sum::<f64>();
            balance_path::read(&json!({"amount":amount}), "$amount")
        }
        Reader::AiHubMix => {
            if number(&reply["total_usage"]).is_some_and(|number| number < 0.0) {
                return Err(PluginError::invalid("This API key has no separate balance. Set the account balance token to read the account's balance."));
            }
            balance_path::read(reply, "$total_usage")
        }
    };
    result.map_err(PluginError::invalid)
}
fn number(value: &Value) -> Option<f64> {
    value
        .as_f64()
        .or_else(|| value.as_str()?.parse().ok())
        .filter(|value| value.is_finite())
}
fn missing() -> PluginError {
    PluginError::invalid("The provider did not return a readable balance amount.")
}
