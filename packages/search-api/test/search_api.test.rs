use super::*;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};

#[test]
fn all_seven_requests_and_parsers_match_the_reference_vendor_contracts() {
    for (vendor, method, path, payload, expected) in [
        (
            Vendor::Tavily,
            reqwest::Method::POST,
            "/search",
            json!({"results":[{"title":"A &amp; <b>B</b>","url":"https://example.com","content":"body"}]}),
            "body",
        ),
        (
            Vendor::Brave,
            reqwest::Method::GET,
            "/res/v1/web/search",
            json!({"web":{"results":[{"url":"https://example.com","description":"brave"}]}}),
            "brave",
        ),
        (
            Vendor::Exa,
            reqwest::Method::POST,
            "/search",
            json!({"results":[{"url":"https://example.com","text":"exa"}]}),
            "exa",
        ),
        (
            Vendor::Firecrawl,
            reqwest::Method::POST,
            "/v2/search",
            json!({"data":{"web":[{"url":"https://example.com","markdown":"firecrawl"}]}}),
            "firecrawl",
        ),
        (
            Vendor::You,
            reqwest::Method::POST,
            "/v1/search",
            json!({"results":{"web":[{"url":"https://example.com","snippets":["one","two"]}],"news":[{"url":"https://news.example.com","description":"news"}]}}),
            "one two",
        ),
        (
            Vendor::DeepSeek,
            reqwest::Method::POST,
            "/v1/tools/search",
            json!({"results":[{"url":"https://example.com","summary":"summary","snippet":"short"}]}),
            "summary",
        ),
        (
            Vendor::SearXNG,
            reqwest::Method::GET,
            "/search",
            json!({"results":[{"url":"https://example.com","content":"searxng"}]}),
            "searxng",
        ),
    ] {
        let (actual_method, actual_path, body, params) = request(vendor, "hello", false);
        assert_eq!((actual_method, actual_path), (method, path));
        if vendor == Vendor::Tavily {
            assert_eq!(
                body.unwrap(),
                json!({"query":"hello","max_results":6,"search_depth":"basic"})
            );
        }
        if vendor == Vendor::Brave {
            assert_eq!(params, vec![("q", "hello".into()), ("count", "6".into())]);
        }
        let pages = parse(vendor, &payload).unwrap();
        assert_eq!(pages[0].text, expected);
        if vendor == Vendor::Tavily {
            assert_eq!(pages[0].title, "A & B");
        }
        if vendor == Vendor::You {
            assert_eq!(pages.len(), 2);
        }
        let result = pages_result(pages);
        assert!(result["text"]
            .as_str()
            .unwrap()
            .contains("https://example.com"));
    }
    assert_eq!(
        parse(
            Vendor::Firecrawl,
            &json!({"data":[{"url":"https://example.com","markdown":"v1"}]})
        )
        .unwrap()[0]
            .text,
        "v1"
    );
    let (_, path, _, params) = request(Vendor::You, "query", true);
    assert_eq!(path, "/v1/agents/search");
    assert_eq!(params[0], ("query", "query".into()));
    let (_, _, body, _) = request(Vendor::DeepSeek, &"搜".repeat(200), false);
    assert_eq!(
        body.unwrap()["query"].as_str().unwrap().chars().count(),
        128
    );
}

#[test]
fn pages_are_plain_bounded_and_only_http_sources_are_public() {
    assert_eq!(
        plain("<strong>A</strong> &#x4e2d; &amp; B\n  C"),
        "A 中 & B C"
    );
    let pages=parse(Vendor::Tavily,&json!({"results":[{"url":"javascript:bad"},{"url":"https://u:secret@example.com"},{"url":"https://example.com","content":"中".repeat(1000)}]})).unwrap();
    assert_eq!(pages.len(), 1);
    assert_eq!(pages[0].text.len(), 1503);
    assert!(pages[0].text.ends_with('…'));
    let pages=parse(Vendor::Tavily,&json!({"results":(0..12).map(|_|json!({"url":"https://example.com"})).collect::<Vec<_>>()})).unwrap();
    assert_eq!(pages.len(), 6);
    assert!(parse(Vendor::You, &json!({})).is_err());
    for base in [
        "https://u:secret@example.com",
        "file:///tmp/search",
        "https://example.com?key=secret",
        "https://example.com/#fragment",
    ] {
        assert!(validate_base(base).is_err());
    }
}

#[test]
fn refused_keys_rest_by_reference_status_and_retry_after_and_are_tried_last() {
    let now = SystemTime::now();
    for (vendor, status, expected) in [
        (Vendor::Tavily, 401, 600),
        (Vendor::You, 403, 600),
        (Vendor::Tavily, 432, 3600),
        (Vendor::Exa, 432, 60),
        (Vendor::DeepSeek, 402, 3600),
        (Vendor::You, 429, 60),
    ] {
        assert_eq!(
            rest_for(vendor, status, None, now),
            Duration::from_secs(expected)
        );
    }
    assert_eq!(
        rest_for(Vendor::Tavily, 429, Some("0"), now),
        Duration::from_secs(30)
    );
    assert_eq!(
        rest_for(Vendor::Tavily, 429, Some("99999"), now),
        Duration::from_secs(600)
    );
    let date = httpdate::fmt_http_date(now + Duration::from_secs(120));
    assert!((119..=120).contains(&rest_for(Vendor::You, 429, Some(&date), now).as_secs()));
    let keys = keys("first, second;first\nthird");
    assert_eq!(keys, vec!["first", "second", "third"]);
    let mut pool = Pool::default();
    let now = Instant::now();
    assert_eq!(pool.order(&keys, false, now), vec![0, 1, 2]);
    assert_eq!(pool.order(&keys, false, now), vec![1, 2, 0]);
    pool.rest("first", Duration::from_secs(600), now);
    pool.rest("third", Duration::from_secs(30), now);
    assert_eq!(pool.order(&keys, true, now), vec![1, 2, 0]);
    assert!(pool.rests.keys().all(|key| !keys.contains(key)));
}

async fn server(
    responses: Vec<(u16, &'static str, &'static str)>,
) -> (String, tokio::task::JoinHandle<Vec<String>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let task = tokio::spawn(async move {
        let mut requests = Vec::new();
        for (status, headers, body) in responses {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            let mut byte = [0];
            while !request.ends_with(b"\r\n\r\n") {
                socket.read_exact(&mut byte).await.unwrap();
                request.push(byte[0]);
            }
            let head = String::from_utf8(request.clone()).unwrap();
            let length = head
                .lines()
                .filter_map(|line| line.split_once(':'))
                .find(|(name, _)| name.eq_ignore_ascii_case("content-length"))
                .and_then(|(_, value)| value.trim().parse::<usize>().ok())
                .unwrap_or_default();
            let mut body_bytes = vec![0; length];
            socket.read_exact(&mut body_bytes).await.unwrap();
            request.extend(body_bytes);
            requests.push(String::from_utf8(request).unwrap());
            socket.write_all(format!("HTTP/1.1 {status} Response\r\nContent-Length: {}\r\nContent-Type: application/json\r\nConnection: close\r\n{headers}\r\n{body}",body.len()).as_bytes()).await.unwrap();
        }
        requests
    });
    (base, task)
}
fn initialize(base: &str, ordered: bool) -> InitializeParams {
    InitializeParams {
        api_version: 1,
        subscription_accounts_enabled: true,
        plugin_id: "tavily".into(),
        plugin_directory: Default::default(),
        data_directory: Default::default(),
        manifest: json!({"authMethods":[{"id":"apiKey","type":"api","label":"API key","fields":[{"key":"apiKey","type":"password","label":"API key","required":true}]}]}),
        installed_manifest: None,
        runtime_config: json!({"baseURL":base,"searchKeysInOrder":ordered}),
    }
}
#[tokio::test]
async fn real_local_http_uses_vendor_auth_failover_and_cooldown_without_returning_secrets() {
    let (base,task)=server(vec![(401,"","{\"error\":\"first-private-key\"}"),(200,"","{\"results\":[{\"title\":\"Page\",\"url\":\"https://example.com\",\"content\":\"verified\"}]}"),(200,"","{\"results\":[{\"url\":\"https://example.com\"}]}")]).await;
    let plugin = SearchApiPlugin::new(Vendor::Tavily);
    plugin.initialize(initialize(&base, true)).await.unwrap();
    let context = CallContext {
        credentials: Some(json!({"type":"apiKey","key":"first-private-key,second-private-key"})),
        network_proxy: Some(nora_provider_plugin_sdk::NetworkProxy::Direct),
        ..Default::default()
    };
    let result = plugin
        .web_search(context.clone(), "query".into())
        .await
        .unwrap();
    assert!(result["text"].as_str().unwrap().contains("verified"));
    assert!(!result.to_string().contains("private-key"));
    plugin.web_search(context, "again".into()).await.unwrap();
    let requests = task.await.unwrap();
    assert!(requests[0].contains("Bearer first-private-key"));
    assert!(requests[1].contains("Bearer second-private-key"));
    assert!(requests[2].contains("Bearer second-private-key"));
    assert!(requests
        .iter()
        .all(|request| request.starts_with("POST /search ")));
}
#[tokio::test]
async fn non_key_failures_stop_before_another_key_and_redirects_never_receive_credentials() {
    for status in [400, 500, 302] {
        let (base, task) = server(vec![(
            status,
            "Location: http://127.0.0.1:1/private\r\n",
            "{\"error\":\"private-key\"}",
        )])
        .await;
        let plugin = SearchApiPlugin::new(Vendor::Brave);
        plugin.initialize(initialize(&base, false)).await.unwrap();
        let error = plugin
            .web_search(
                CallContext {
                    credentials: Some(json!({"type":"apiKey","key":"private-key,unused-key"})),
                    network_proxy: Some(nora_provider_plugin_sdk::NetworkProxy::Direct),
                    ..Default::default()
                },
                "query".into(),
            )
            .await
            .unwrap_err();
        assert!(!error.to_string().contains("private-key"));
        let requests = task.await.unwrap();
        assert_eq!(requests.len(), 1);
        assert!(requests[0]
            .to_lowercase()
            .contains("x-subscription-token: private-key"));
    }
}

#[tokio::test]
async fn same_origin_redirects_keep_auth_and_complete_like_the_reference() {
    let (base, task) = server(vec![
        (302, "Location: /redirected\r\n", ""),
        (200, "", "{\"results\":[{\"url\":\"https://example.com\"}]}"),
    ])
    .await;
    let plugin = SearchApiPlugin::new(Vendor::Tavily);
    plugin.initialize(initialize(&base, false)).await.unwrap();
    plugin
        .web_search(
            CallContext {
                credentials: Some(json!({"type":"apiKey","key":"private-key"})),
                network_proxy: Some(nora_provider_plugin_sdk::NetworkProxy::Direct),
                ..Default::default()
            },
            "query".into(),
        )
        .await
        .unwrap();
    let requests = task.await.unwrap();
    assert!(requests[0].starts_with("POST /search "));
    assert!(requests[1].starts_with("GET /redirected "));
    assert!(requests[1].contains("Bearer private-key"));
}

#[tokio::test]
async fn all_search_packages_expose_their_declared_auth_form_and_only_accept_api_key_accounts() {
    for (vendor, raw_manifest) in [
        (
            Vendor::Tavily,
            include_str!("../../tavily/nora-provider.json"),
        ),
        (
            Vendor::Brave,
            include_str!("../../brave-search/nora-provider.json"),
        ),
        (
            Vendor::Exa,
            include_str!("../../exa-search/nora-provider.json"),
        ),
        (
            Vendor::Firecrawl,
            include_str!("../../firecrawl/nora-provider.json"),
        ),
        (
            Vendor::You,
            include_str!("../../you-search/nora-provider.json"),
        ),
        (
            Vendor::DeepSeek,
            include_str!("../../deepseek-search/nora-provider.json"),
        ),
        (
            Vendor::SearXNG,
            include_str!("../../searxng/nora-provider.json"),
        ),
    ] {
        let manifest: Value = serde_json::from_str(raw_manifest).unwrap();
        let plugin = SearchApiPlugin::new(vendor);
        let initialized = plugin
            .initialize(InitializeParams {
                api_version: 1,
                plugin_id: manifest["id"].as_str().unwrap().into(),
                manifest: manifest.clone(),
                runtime_config: json!({}),
                subscription_accounts_enabled: false,
                installed_manifest: Some(manifest.clone()),
                plugin_directory: Default::default(),
                data_directory: Default::default(),
            })
            .await
            .unwrap();
        assert_eq!(initialized["apiVersion"], 1);
        assert_eq!(
            plugin.auth_methods(CallContext::default()).await.unwrap(),
            manifest["authMethods"],
            "{vendor:?}: the UI form must match the installed package"
        );
        assert_eq!(manifest["config"]["keyOptional"], vendor.optional_key());
        let saved = plugin
            .authorize(json!({"method":"apiKey","inputs":{"apiKey":"  offline-private-key  "}}))
            .await
            .unwrap();
        assert_eq!(
            saved["credentials"],
            json!({"type":"apiKey","authMethod":"apiKey","key":"offline-private-key"})
        );
        assert!(plugin
            .authorize(json!({"method":"oauth","inputs":{"apiKey":"private"}}))
            .await
            .is_err());
        assert!(plugin
            .authorize(json!({"method":42,"inputs":{"apiKey":"private"}}))
            .await
            .is_err());
        assert!(plugin
            .authorize(json!({"method":"apiKey","inputs":{"apiKey":42}}))
            .await
            .is_err());
        assert_eq!(
            plugin
                .authorize(json!({"method":"apiKey","inputs":{"apiKey":""}}))
                .await
                .is_ok(),
            vendor.optional_key()
        );
        assert!(plugin
            .accounts(CallContext::default())
            .await
            .unwrap()
            .as_array()
            .unwrap()
            .is_empty());
        assert!(plugin
            .models(CallContext::default())
            .await
            .unwrap()
            .as_array()
            .unwrap()
            .is_empty());
    }
}
