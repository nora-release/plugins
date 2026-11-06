use super::*;

#[test]
fn search_source_dates_survive_each_vendor_shape_without_becoming_answer_text() {
    for (vendor, payload, dates) in [
        (
            Vendor::DeepSeek,
            json!({"results":[{"url":"https://example.com/a","title":"A","summary":"Summary","date":"2026-10-01T00:00:00+00:00"}]}),
            vec!["2026-10-01T00:00:00+00:00"],
        ),
        (
            Vendor::You,
            json!({"results":{"web":[{"url":"https://example.com/a","title":"A","snippets":["Summary"],"page_age":"1 day ago"}],"news":[{"url":"https://example.com/b","page_age":"October 1, 2026"}]}}),
            vec!["1 day ago", "October 1, 2026"],
        ),
        (
            Vendor::Tavily,
            json!({"results":[{"url":"https://example.com/a","date":"2026-10-01"}]}),
            vec!["2026-10-01"],
        ),
    ] {
        let result = pages_result(parse(vendor, &payload).unwrap());
        for (source, date) in result["sources"].as_array().unwrap().iter().zip(dates) {
            assert_eq!(source["page_age"], date);
            assert!(!result["text"].as_str().unwrap().contains(date));
        }
    }
}

#[test]
fn absent_or_invalid_dates_do_not_replace_valid_search_results() {
    for date in [
        Value::Null,
        json!(""),
        json!(42),
        json!({"secret":"not-a-date"}),
        json!("x".repeat(4097)),
        json!("date\nprivate"),
    ] {
        for (vendor, payload) in [
            (
                Vendor::DeepSeek,
                json!({"results":[{"url":"https://example.com","summary":"Content","date":date}]}),
            ),
            (
                Vendor::You,
                json!({"results":{"web":[{"url":"https://example.com","snippets":["Content"],"page_age":date}]}}),
            ),
        ] {
            let result = pages_result(parse(vendor, &payload).unwrap());
            assert_eq!(result["sources"].as_array().unwrap().len(), 1);
            assert!(result["sources"][0].get("page_age").is_none());
            assert!(result["text"].as_str().unwrap().ends_with("Content"));
        }
    }
}

#[tokio::test]
async fn actual_search_http_keeps_the_vendors_date_and_only_public_fields() {
    for (vendor, payload) in [
        (
            Vendor::DeepSeek,
            json!({"results":[{"url":"https://example.com/a","date":"2026-10-01","secret":"private"}]}),
        ),
        (
            Vendor::You,
            json!({"results":{"web":[{"url":"https://example.com/a","page_age":"2026-10-01","secret":"private"}]}}),
        ),
    ] {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = vec![0; 8192];
            assert!(socket.read(&mut request).await.unwrap() > 0);
            let body = payload.to_string();
            socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",body.len(),body).as_bytes()).await.unwrap();
        });
        let plugin = SearchApiPlugin::new(vendor);
        plugin.initialize(initialize(&base, false)).await.unwrap();
        let result = plugin
            .web_search(
                CallContext {
                    credentials: Some(json!({"type":"apiKey","key":"fixture-key"})),
                    network_proxy: Some(nora_provider_plugin_sdk::NetworkProxy::Direct),
                    ..Default::default()
                },
                "query".into(),
            )
            .await
            .unwrap();
        assert_eq!(result["sources"][0]["page_age"], "2026-10-01");
        assert!(!result.to_string().contains("private"));
        server.await.unwrap();
    }
}
