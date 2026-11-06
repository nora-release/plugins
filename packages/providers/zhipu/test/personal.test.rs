use super::*;
use async_trait::async_trait;
use futures::stream;
use nora_ai::{HttpClientError, HttpMethod, HttpRequest, HttpStreamingResponse};
use std::collections::BTreeMap;
use std::sync::Mutex;

struct Client {
    requests: Mutex<Vec<HttpRequest>>,
    resets: (u16, Vec<u8>),
    subscriptions: (u16, Vec<u8>),
    reset_hangs: bool,
    quota_delay: Duration,
}
impl Client {
    fn new(resets: &Value, subscriptions: &Value) -> Self {
        Self {
            requests: Mutex::default(),
            resets: (200, serde_json::to_vec(resets).unwrap()),
            subscriptions: (200, serde_json::to_vec(subscriptions).unwrap()),
            reset_hangs: false,
            quota_delay: Duration::ZERO,
        }
    }
}
#[async_trait]
impl HttpClient for Client {
    async fn stream(&self, request: HttpRequest) -> Result<HttpStreamingResponse, HttpClientError> {
        let (status, body) = if request.url.ends_with("/api/monitor/usage/quota/limit") {
            tokio::time::sleep(self.quota_delay).await;
            (200, include_bytes!("fixtures/usage.json").to_vec())
        } else if request
            .url
            .ends_with("/api/biz/customer-package-reset/list?targetType=PERSONAL")
        {
            if self.reset_hangs {
                self.requests.lock().unwrap().push(request);
                return std::future::pending().await;
            }
            self.resets.clone()
        } else if request.url.ends_with("/api/biz/subscription/list") {
            self.subscriptions.clone()
        } else {
            panic!("Unexpected usage endpoint: {}", request.url);
        };
        self.requests.lock().unwrap().push(request);
        Ok(HttpStreamingResponse {
            status,
            headers: BTreeMap::new(),
            body: Box::pin(stream::iter(vec![Ok(body.into())])),
        })
    }
}

#[tokio::test(start_paused = true)]
async fn a_hanging_optional_endpoint_preserves_the_quota_and_other_completed_details() {
    let subscriptions: Value =
        serde_json::from_str(include_str!("fixtures/personal_subscriptions.json")).unwrap();
    let mut client = Client::new(&Value::Null, &subscriptions);
    client.reset_hangs = true;
    let value = tokio::time::timeout(Duration::from_secs(12), read("api.z.ai", &client))
        .await
        .expect("Optional details must finish before the shortest host usage deadline");
    assert_eq!(value["items"][0]["usedPercent"], 12.0);
    assert_eq!(value["until"], "2026-11-18T02:00:00+00:00");
    assert!(value["resetCredits"].is_null());
}

#[tokio::test(start_paused = true)]
async fn a_ten_second_primary_read_survives_the_shortest_host_deadline_despite_optional_hangs() {
    for delay in [Duration::from_secs(10), Duration::from_millis(9500)] {
        let mut client = Client::new(&Value::Null, &Value::Null);
        client.quota_delay = delay;
        client.reset_hangs = true;
        let started = Instant::now();
        let value = tokio::time::timeout(Duration::from_secs(12), read("api.z.ai", &client))
            .await
            .expect("A successful primary read must return even when optional details hang");
        assert_eq!(value["items"][0]["usedPercent"], 12.0);
        assert_eq!(value["available"], true);
        assert!(value["resetCredits"].is_null());
        assert!(started.elapsed() < Duration::from_secs(12));
        let requests = client.requests.lock().unwrap();
        if delay >= PERSONAL_USAGE_BUDGET {
            assert_eq!(requests.len(), 1);
        } else {
            assert_eq!(requests.len(), 3);
        }
    }
}

async fn read(host: &str, client: &Client) -> Value {
    let auth = ResolvedProviderAuth {
        token: Some("private-glm-key".into()),
        base_url: Some(format!("https://{host}/api/coding/paas/v4")),
        ..Default::default()
    };
    super::super::team::usage(
        &auth,
        host,
        &format!("https://{host}/api/monitor/usage/quota/limit"),
        client,
    )
    .await
    .unwrap()
}

#[tokio::test]
async fn personal_usage_keeps_reset_types_expiries_and_next_renewal_on_both_sites() {
    let resets: Value =
        serde_json::from_str(include_str!("fixtures/personal_resets.json")).unwrap();
    let subscriptions: Value =
        serde_json::from_str(include_str!("fixtures/personal_subscriptions.json")).unwrap();
    for host in ["open.bigmodel.cn", "api.z.ai"] {
        let client = Client::new(&resets, &subscriptions);
        let value = read(host, &client).await;
        assert_eq!(value["items"][0]["usedPercent"], 12.0);
        assert_eq!(value["items"][1]["usedPercent"], 40.0);
        assert_eq!(value["items"][2]["countsForRouting"], false);
        let credits = &value["resetCredits"];
        assert_eq!(credits["totalAvailable"], 2);
        assert_eq!(credits["grants"][0]["window"], "weekly");
        assert_eq!(
            credits["grants"][0]["expiresAt"],
            "2026-10-30T15:59:59+00:00"
        );
        assert_eq!(credits["grants"][1]["window"], "fiveHour");
        assert_eq!(
            credits["grants"][1]["expiresAt"],
            "2026-11-02T15:59:59+00:00"
        );
        assert_eq!(value["until"], "2026-11-18T02:00:00+00:00");
        assert_eq!(value["renew"], "auto");
        let requests = client.requests.lock().unwrap();
        assert_eq!(requests.len(), 3);
        for request in requests.iter() {
            assert_eq!(request.method, HttpMethod::Get);
            assert!(request.url.starts_with(&format!("https://{host}/")));
            assert_eq!(request.headers["authorization"], "private-glm-key");
            assert!(request.body.is_none());
            assert!(!request.headers.contains_key("Bigmodel-Organization"));
        }
        assert!(!value.to_string().contains("private-glm-key"));
        assert!(!value.to_string().contains("customerId"));
        assert!(!value.to_string().contains("recordId"));
    }
}

#[tokio::test]
async fn optional_failures_and_unknown_reset_shapes_do_not_fabricate_counts_or_hide_usage() {
    for (status, body) in [
        (403, br#"{"success":false}"#.to_vec()),
        (200, b"<html>502</html>".to_vec()),
        (
            200,
            br#"{"code":1000,"success":false,"msg":"private-glm-key"}"#.to_vec(),
        ),
        (200, br#"{"code":200,"success":true,"data":null}"#.to_vec()),
        (200, br#"{"code":200,"data":{}}"#.to_vec()),
        (
            200,
            br#"{"code":200,"data":{"fiveHourResets":[]}}"#.to_vec(),
        ),
        (
            200,
            br#"{"code":200,"data":{"fiveHourResets":[],"weekResets":null}}"#.to_vec(),
        ),
        (
            200,
            br#"{"code":200,"data":{"fiveHourResets":[],"weekResets":[{}]}}"#.to_vec(),
        ),
        (
            200,
            br#"{"code":200,"data":{"fiveHourResets":[],"weekResets":[{"available":"false"}]}}"#
                .to_vec(),
        ),
    ] {
        let mut client = Client::new(&Value::Null, &Value::Null);
        client.resets = (status, body.clone());
        client.subscriptions = (status, body);
        let value = read("api.z.ai", &client).await;
        assert_eq!(value["available"], true);
        assert_eq!(value["items"][0]["usedPercent"], 12.0);
        assert!(value["resetCredits"].is_null());
        assert!(value.get("until").is_none());
        assert!(value.get("renew").is_none());
        assert!(!value.to_string().contains("private-glm-key"));
    }
}

#[tokio::test]
async fn valid_empty_or_spent_reset_lists_are_known_zero_on_both_sites() {
    for host in ["open.bigmodel.cn", "api.z.ai"] {
        for data in [
            json!({"fiveHourResets":[],"weekResets":[]}),
            json!({"fiveHourResets":[{"available":false,"expireTime":"2026-10-04 23:59:59"}],"weekResets":[{"available":false}]}),
        ] {
            let client = Client::new(
                &json!({"code":200,"success":true,"data":data}),
                &Value::Null,
            );
            let usage = read(host, &client).await;
            assert_eq!(usage["items"][0]["usedPercent"], 12.0);
            assert_eq!(
                usage["resetCredits"],
                json!({"available":true,"totalAvailable":0,"grants":[]})
            );
        }
    }
}

#[tokio::test]
async fn a_nonrenewing_term_uses_the_last_valid_date_and_an_unknown_auto_charge_stays_unknown() {
    let resets = json!({"code":200,"data":{"fiveHourResets":[{"available":true,"expireTime":1793375999000u64}],"weekResets":[{"available":true}]}});
    let subscriptions = json!({"code":200,"data":[{"status":"VALID","autoRenew":false,"valid":"2026-09-18 10:00:00-2026-12-18 10:00:00"}]});
    let client = Client::new(&resets, &subscriptions);
    let value = read("open.bigmodel.cn", &client).await;
    assert_eq!(value["until"], "2026-12-18T02:00:00+00:00");
    assert_eq!(value["renew"], "off");
    assert_eq!(value["resetCredits"]["totalAvailable"], 2);
    assert_eq!(
        value["resetCredits"]["grants"][0]["expiresAt"],
        "2026-10-30T15:59:59+00:00"
    );
    assert!(value["resetCredits"]["grants"][1]
        .get("expiresAt")
        .is_none());

    let subscriptions = json!({"code":200,"data":[{"status":"VALID","autoRenew":true,"valid":"2026-09-18-2026-12-18"}]});
    let value = read("api.z.ai", &Client::new(&Value::Null, &subscriptions)).await;
    assert!(value.get("until").is_none());
    assert!(value.get("renew").is_none());
}

#[test]
fn plan_dates_keep_beijing_time_and_numeric_seconds() {
    assert_eq!(
        when(&json!("2026-12-18")),
        Some("2026-12-17T16:00:00+00:00".into())
    );
    assert_eq!(
        when(&json!("1793375999")),
        Some("2026-10-30T15:59:59+00:00".into())
    );
    assert_eq!(when(&json!("not a date")), None);
}
