use super::*;
use crate::browser::tests::{offline_browser, route, Directory};
use std::collections::BTreeMap;
fn now() -> i64 {
    chrono::DateTime::parse_from_rfc3339("2026-10-18T12:00:00Z")
        .unwrap()
        .timestamp_millis()
}
fn headers() -> Value {
    json!([
        "Credit name",
        "Status",
        "Discount",
        "Remaining value",
        "Original value",
        "Type",
        "Credit ID",
        "Usage scope",
        "Start date",
        "End date"
    ])
}
fn row(id: &str, remaining: &str, original: &str, status: &str) -> Value {
    json!([
        "Example benefit",
        status,
        "100%",
        remaining,
        original,
        "One-time",
        id,
        "All Google Cloud services",
        "July 15, 2026",
        "July 1, 2027"
    ])
}
fn settings(directory: &Directory) -> Settings {
    Settings::resolve_with_env(&json!({"project":"example-project","billingAccount":"AAAAAA-BBBBBB-CCCCCC","account":"first@example.com","profile":directory.0.join("profile")}),None,&directory.0,&BTreeMap::from([("HOME".into(),directory.0.to_string_lossy().into_owned()),("XDG_CACHE_HOME".into(),directory.0.to_string_lossy().into_owned())])).unwrap()
}
#[test]
fn sums_only_eligible_rows_deduplicates_ids_counts_spent_and_uses_utc_expiry() {
    let mut future = row("future", "$10.00", "$10.00", "Available");
    future[8] = json!("January 1, 2027");
    let out=parse_tables(&json!([{"headers":headers(),"rows":[row("a","$10.00","$10.00","Available"),row("b","$2.07","$10.00","Active"),row("b","$2.07","$10.00","Active"),row("expired","$262.09","$300.00","Expired"),future,row("used","$1.00","$10.00","Exhausted")]}]),now()).unwrap();
    assert_eq!(out["remaining"], 12.07);
    assert_eq!(out["original"], 30.0);
    assert_eq!(out["used"], 17.93);
    assert_eq!(out["runsOutAt"], "2027-07-01T00:00:00.000Z");
    assert_eq!(out["runsOut"], 12.07);
    assert_eq!(out["credits"].as_array().unwrap().len(), 5);
    assert_eq!(out["credits"][0]["id"], "a");
    assert_eq!(out["cached"], false);
}
#[test]
fn statuses_currencies_amounts_and_missing_layout_fail_instead_of_reporting_zero() {
    for rows in [
        vec![row("a", "€1.00", "$10.00", "Available")],
        vec![row("a", "$11.00", "$10.00", "Active")],
        vec![row("a", "$1.00", "$10.00", "Mystery")],
        vec![row("a", "$-1.00", "$10.00", "Available")],
    ] {
        assert!(parse_tables(&json!([{"headers":headers(),"rows":rows}]), now()).is_err());
    }
    assert!(parse_tables(&json!([]), now()).is_err());
    assert!(parse_tables(&json!([{"headers":["Other"],"rows":[["$100.00"]]}]), now()).is_err());
    for status in ["Used", "Used up", "Exhausted"] {
        let out = parse_tables(
            &json!([{"headers":headers(),"rows":[row("used","$1.00","$10.00",status)]}]),
            now(),
        )
        .unwrap();
        assert_eq!(out["remaining"], 0.0);
        assert_eq!(out["usedPercent"], 100.0);
    }
}
#[test]
fn table_headers_are_matched_by_name_and_dates_and_amounts_are_strict() {
    let out=parse_tables(&json!([{"headers":["Credit ID","Original value","Remaining value","Status","Credit name"],"rows":[["id","US$1,234.56","USD 100.01","Available","Benefit"]]}]),now()).unwrap();
    assert_eq!(out["remaining"], 100.01);
    assert_eq!(out["original"], 1234.56);
    assert!(money("$1.1").is_err());
    assert!(money("$99999999999999999999.00").is_err());
    assert!(date("not a date").is_err());
    assert_eq!(date("2026-10-18T12:00:00Z").unwrap(), Some(now()));
}
#[test]
fn cache_keeps_failures_for_different_lifetimes_and_never_reuses_expired_balance() {
    let directory = Directory::new();
    let mut settings = settings(&directory);
    let file = cache_file(&settings);
    let at = now();
    cache::write(&file,&json!({"currency":"USD","remaining":10,"original":20,"usedPercent":50,"fetchedAt":iso(at)})).unwrap();
    assert_eq!(
        cached(&file, &settings, at + 1000).unwrap().unwrap()["cached"],
        true
    );
    assert!(cached(&file, &settings, at - 1).is_none());
    assert!(cached(&file, &settings, at + 301000).is_none());
    cache::write(&file,&json!({"currency":"USD","remaining":10,"original":20,"usedPercent":50,"fetchedAt":iso(at),"runsOutAt":iso(at+1000)})).unwrap();
    assert!(cached(&file, &settings, at + 1000).is_none());
    for (error, seconds) in [
        ("Cloud Console permission denied", 1200),
        ("net::ERR_CONNECTION_RESET", 60),
    ] {
        cache::write(&file, &json!({"error":error,"failedAt":iso(at)})).unwrap();
        assert_eq!(
            cached(&file, &settings, at + 1000)
                .unwrap()
                .unwrap_err()
                .message,
            error
        );
        assert!(cached(&file, &settings, at + seconds * 1000).is_none());
    }
    settings.force = true;
    assert!(cached(&file, &settings, at).is_none());
}
#[test]
fn cache_identity_separates_google_accounts_profiles_billing_and_adc_directories() {
    let directory = Directory::new();
    let settings = settings(&directory);
    let file = cache_file(&settings);
    for key in ["account", "profile", "project", "billing", "adc"] {
        let mut other = settings.clone();
        match key {
            "account" => other.account = Some("second@example.com".into()),
            "profile" => other.profile = directory.0.join("other"),
            "project" => other.project = Some("another-project".into()),
            "billing" => other.billing_account = Some("AAAAAA-BBBBBB-DDDDDD".into()),
            _ => other.cloudsdk_config = Some(directory.0.join("adc")),
        }
        assert_ne!(cache_file(&other), file, "{key}");
    }
}
#[tokio::test]
async fn cached_success_and_failure_do_not_open_chrome_or_change_other_accounts() {
    let directory = Directory::new();
    let settings = settings(&directory);
    let file = cache_file(&settings);
    let browser = BrowserService::default();
    cache::write(&file,&json!({"currency":"USD","remaining":10,"original":20,"usedPercent":50,"fetchedAt":iso(chrono::Utc::now().timestamp_millis())})).unwrap();
    assert_eq!(read(&browser, &settings).await.unwrap()["cached"], true);
    cache::write(&file,&json!({"error":"permission denied remembered","failedAt":iso(chrono::Utc::now().timestamp_millis())})).unwrap();
    assert_eq!(
        read(&browser, &settings).await.unwrap_err().message,
        "permission denied remembered"
    );
}
#[tokio::test]
async fn real_chrome_scrape_follows_pagination_confirms_account_and_deduplicates() {
    if browser::chrome_executable().is_err() {
        return;
    }
    let directory = Directory::new();
    let browser = offline_browser(&directory).await;
    let page = browser.new_page().await.unwrap();
    let html = r#"<!doctype html><script>var a={"oPEP7c":"first@example.com"};</script><table><thead><tr><th>Credit name</th><th>Status</th><th>Remaining value</th><th>Original value</th><th>Credit ID</th></tr></thead><tbody><tr><td>Benefit one</td><td>Available</td><td>$10.00</td><td>$10.00</td><td>one</td></tr></tbody></table><button aria-label="Next page" onclick="document.querySelector('tbody').innerHTML='<tr><td>Benefit two</td><td>Available</td><td>$2.07</td><td>$10.00</td><td>two</td></tr>';this.disabled=true">Next</button>"#;
    let routing = route(&page, html).await;
    let settings = settings(&directory);
    let out = scrape(page, &settings).await.unwrap();
    assert_eq!(out["remaining"], 12.07);
    assert_eq!(out["original"], 20.0);
    assert_eq!(out["billingAccount"], "AAAAAA-BBBBBB-CCCCCC");
    assert!(out["source"]
        .as_str()
        .unwrap()
        .contains("authuser=first%40example.com"));
    routing.abort();
    browser.close().await;
}
#[tokio::test]
async fn real_console_never_reports_another_users_balance() {
    if browser::chrome_executable().is_err() {
        return;
    }
    let directory = Directory::new();
    let browser = offline_browser(&directory).await;
    let page = browser.new_page().await.unwrap();
    let html = r#"<!doctype html><script>var a={"oPEP7c":"other@example.com"};</script><table><thead><tr><th>Remaining value</th></tr></thead><tbody><tr><td>$10.00</td></tr></tbody></table>"#;
    let routing = route(&page, html).await;
    assert!(scrape(page, &settings(&directory))
        .await
        .unwrap_err()
        .message
        .contains("other@example.com"));
    routing.abort();
    browser.close().await;
}
#[tokio::test]
async fn actual_dom_parser_runs_without_javascript_or_external_resources() {
    if browser::chrome_executable().is_err() {
        return;
    }
    let directory = Directory::new();
    let browser = offline_browser(&directory).await;
    let page = browser.new_page().await.unwrap();
    page.send(
        "Emulation.setScriptExecutionDisabled",
        json!({"value":true}),
    )
    .await
    .unwrap();
    page.send("Network.enable", json!({})).await.unwrap();
    page.send("Network.setBlockedURLs", json!({"urls":["*"]}))
        .await
        .unwrap();
    let frame = page.send("Page.getFrameTree", json!({})).await.unwrap();
    page.send("Page.setDocumentContent",json!({"frameId":frame["frameTree"]["frame"]["id"],"html":"<script>window.ran=true;fetch('https://evil.test')</script><table><thead><tr><th>Credit name</th><th>Status</th><th>Remaining value</th><th>Original value</th><th>Credit ID</th></tr></thead><tbody><tr><td>Benefit</td><td>Available</td><td>$10.00</td><td>$20.00</td><td>one</td></tr></tbody></table>"})).await.unwrap();
    let tables = page.evaluate(TABLES).await.unwrap();
    assert_eq!(parse_tables(&tables, now()).unwrap()["remaining"], 10.0);
    assert_eq!(page.evaluate("window.ran===true").await.unwrap(), false);
    browser.close().await;
}

#[tokio::test]
async fn real_billing_discovery_requires_one_project_account_and_keyed_identity() {
    if browser::chrome_executable().is_err() {
        return;
    }
    let directory = Directory::new();
    let browser = offline_browser(&directory).await;
    let page = browser.new_page().await.unwrap();
    let routing=route(&page,"<!doctype html><h1>Google Cloud Console</h1><p>Contact billing@example.com</p><a href='/billing/AAAAAA-BBBBBB-CCCCCC'>Billing account</a>").await;
    let mut settings = settings(&directory);
    settings.billing_account = None;
    assert_eq!(
        billing(&page, &settings).await.unwrap(),
        "AAAAAA-BBBBBB-CCCCCC"
    );
    assert!(browser::account(&page, Duration::from_millis(150))
        .await
        .is_err());
    routing.abort();
    page.send("Fetch.disable", json!({})).await.unwrap();
    let routing=route(&page,"<!doctype html><h1>Google Cloud Console</h1><a href='/billing/AAAAAA-BBBBBB-CCCCCC'>One</a><a href='/billing/AAAAAA-BBBBBB-DDDDDD'>Two</a>").await;
    assert!(billing(&page, &settings)
        .await
        .unwrap_err()
        .message
        .contains("uniquely"));
    routing.abort();
    browser.close().await;
}
