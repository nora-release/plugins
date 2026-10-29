use super::*;
use crate::test_support::{Client, Directory};
use std::collections::BTreeMap;

#[test]
fn only_chat_gemini_models_are_offered_and_native_metadata_keeps_signatures_and_capabilities() {
    for id in ["gemini-4-argon", "gemini-3.8-flash", "gemini-2.5-pro"] {
        assert!(is_chat(id));
    }
    for id in [
        "imagen-4",
        "gemini-image",
        "gemini-tts",
        "gemini-live",
        "gemini-embedding",
        "gemini-omni",
        "gemini-translate",
        "gemini-foo/bar",
    ] {
        assert!(!is_chat(id));
    }
    let directory = Directory::new();
    let settings = directory.settings();
    let mut catalog = ModelsCatalog::default();
    let mut old = Model::new("gemini-old", "google-generative-ai", "google-vertex");
    old.info.release_date = Some("2026-01-01".into());
    old.name = "An older model".into();
    old.info.reasoning_efforts = vec!["low".into(), "high".into()];
    catalog.insert(old);
    let mut new = Model::new("gemini-new", "google-generative-ai", "google-vertex");
    new.info.release_date = Some("2026-10-20".into());
    catalog.insert(new);
    let value = model_rows(
        &[
            "gemini-old".into(),
            "gemini-unknown-first".into(),
            "gemini-new".into(),
            "gemini-unknown-last".into(),
        ],
        &settings,
        &catalog,
    )
    .unwrap();
    let rows = value["models"].as_array().unwrap();
    assert_eq!(
        rows.iter()
            .map(|model| model["id"].as_str().unwrap())
            .collect::<Vec<_>>(),
        vec![
            "gemini-new",
            "gemini-old",
            "gemini-unknown-last",
            "gemini-unknown-first"
        ]
    );
    assert_eq!(rows[1]["info"]["reasoningEfforts"], json!(["low", "high"]));
    assert!(rows
        .iter()
        .all(|model| model["api"] == "google-generative-ai" && model["provider"] == "gcp-ai"));
}
#[cfg(unix)]
#[tokio::test]
async fn publisher_catalog_reads_all_pages_and_only_404_removes_a_model() {
    let directory = Directory::new();
    let settings = directory.settings();
    let cli = directory.cli(BTreeMap::new());
    let client = Arc::new(Client::new(|request, _| {
        if let Some(reply) = crate::test_support::user_info(request) {
            return reply;
        }
        if request.url.contains("/publishers/google/models?") {
            if request.url.contains("pageToken=") {
                return (
                    200,
                    json!({"publisherModels":[{"name":"publishers/google/models/gemini-other"},{"name":"publishers/google/models/gemini-image"}]}),
                );
            }
            return (
                200,
                json!({"publisherModels":[{"name":"publishers/google/models/gemini-old"},{"name":"publishers/google/models/gemini-forbidden"},{"name":"publishers/google/models/gemini-rate"}],"nextPageToken":"second+page"}),
            );
        }
        if request.url.contains("gemini-old:") {
            (404, json!({}))
        } else if request.url.contains("gemini-forbidden:") {
            (403, json!({}))
        } else if request.url.contains("gemini-rate:") {
            (429, json!({}))
        } else {
            (200, json!({"totalTokens":1}))
        }
    }));
    let lists = ModelLists::default();
    let value = lists
        .available(settings.clone(), cli.clone(), client.clone())
        .await
        .unwrap();
    assert_eq!(
        value["ids"],
        json!(["gemini-forbidden", "gemini-rate", "gemini-other"])
    );
    let calls = client.calls.lock().unwrap();
    assert_eq!(
        calls
            .iter()
            .filter(|request| request.url.contains("countTokens"))
            .count(),
        4
    );
    assert!(calls
        .iter()
        .all(|request| !request.url.contains("generateContent")));
    assert!(calls
        .iter()
        .any(|request| request.url.contains("pageToken=second%2Bpage")));
    drop(calls);
    let count = client.calls.lock().unwrap().len();
    lists
        .available(settings, cli, client.clone())
        .await
        .unwrap();
    assert_eq!(client.calls.lock().unwrap().len(), count);
}
#[cfg(unix)]
#[tokio::test]
async fn every_model_404_keeps_names_and_account_location_caches_stay_separate() {
    let directory = Directory::new();
    let mut settings = directory.settings();
    let cli = directory.cli(BTreeMap::new());
    let client = Arc::new(Client::new(|request, _| {
        if let Some(reply) = crate::test_support::user_info(request) {
            return reply;
        }
        if request.url.contains("models?") {
            (
                200,
                json!({"publisherModels":[{"name":"publishers/google/models/gemini-one"}]}),
            )
        } else {
            (404, json!({}))
        }
    }));
    let lists = ModelLists::default();
    assert_eq!(
        lists
            .available(settings.clone(), cli.clone(), client.clone())
            .await
            .unwrap()["ids"],
        json!(["gemini-one"])
    );
    settings.location = "europe-west1".into();
    lists
        .available(settings.clone(), cli.clone(), client.clone())
        .await
        .unwrap();
    settings.account = Some("other@example.invalid".into());
    let other = directory.0.join("other-adc");
    std::fs::create_dir_all(&other).unwrap();
    std::fs::write(
        other.join("application_default_credentials.json"),
        br#"{"owner":"other@example.invalid"}"#,
    )
    .unwrap();
    settings.cloudsdk_config = Some(other);
    lists
        .available(settings, cli, client.clone())
        .await
        .unwrap();
    assert_eq!(
        client
            .calls
            .lock()
            .unwrap()
            .iter()
            .filter(|request| request.url.contains("models?"))
            .count(),
        3
    );
}
#[cfg(unix)]
#[tokio::test]
async fn stale_lists_answer_immediately_and_an_unreadable_refresh_does_not_erase_them() {
    let directory = Directory::new();
    let mut settings = directory.settings();
    let cli = directory.cli(BTreeMap::new());
    let client = Arc::new(Client::new(|request, _| {
        if let Some(reply) = crate::test_support::user_info(request) {
            return reply;
        }
        if request.url.contains("models?") {
            (
                200,
                json!({"publisherModels":[{"name":"publishers/google/models/gemini-one"}]}),
            )
        } else {
            (200, json!({}))
        }
    }));
    let lists = ModelLists::default();
    lists
        .available(settings.clone(), cli.clone(), client)
        .await
        .unwrap();
    settings.model_ttl = 0.;
    let mut slow = Client::new(|_, _| (503, json!({})));
    slow.delay = Duration::from_millis(300);
    let slow = Arc::new(slow);
    let now = std::time::Instant::now();
    let value = lists
        .available(settings.clone(), cli.clone(), slow.clone())
        .await
        .unwrap();
    assert!(now.elapsed() < Duration::from_millis(100));
    assert_eq!(value["ids"], json!(["gemini-one"]));
    tokio::time::sleep(Duration::from_millis(450)).await;
    settings.force = true;
    assert_eq!(
        lists.available(settings, cli, slow).await.unwrap()["ids"],
        json!(["gemini-one"])
    );
}
