use super::*;
use crate::tests::{answer, collect, configured, credentials, Client};
use base64::Engine;
fn account(enterprise: bool) -> Value {
    let mut value = credentials();
    value["deviceChat"] = json!(true);
    value["access"] = json!("device-one");
    if enterprise {
        value["enterpriseHost"] = json!("acme.example.com");
        value["vpc"] = json!("acme.example.com");
    }
    value
}
fn catalog() -> Value {
    json!({"app":[{"key":"wrong","enable":true}],"qwork":[{"key":"flash","display_name":"标准","enable":false,"max_input_tokens":131072}]})
}
#[tokio::test]
async fn sign_in_uses_its_own_domain_profile_and_device_token_without_job_exchange() {
    for site in [Site::QwenWork, Site::QwenWorkEnterprise] {
        let client = Client::new(vec![
            (
                200,
                json!({"token":"dt","refresh_token":"dr","user_id":"wrong-id","expires_in":3600}),
            ),
            (
                200,
                json!({"data":{"user":{"id":"u1","email":"one@example.com","name":"One"}}}),
            ),
        ]);
        let provider = configured(site, client.clone());
        let flow = auth::authorize(
            &provider,
            json!({"authMethodID":"oauth","inputs":{"domain":"acme.example.com"}}),
        )
        .await
        .unwrap();
        let url = url::Url::parse(flow["url"].as_str().unwrap()).unwrap();
        let query = url.query_pairs().collect::<BTreeMap<_, _>>();
        assert_eq!(query["challenge_method"], "S256");
        assert_eq!(query["client_id"], "e883ade2-e6e3-4d6d-adf7-f92ceff5fdcb");
        assert_eq!(
            query.get("redirect_uri").map(|v| v.as_ref()),
            if site == Site::QwenWork {
                Some("qwenwork-cn://")
            } else {
                None
            }
        );
        let value = auth::callback(&provider, json!({"session":flow["session"]}))
            .await
            .unwrap();
        let cred = &value["credentials"];
        assert_eq!(cred["uid"], "u1");
        assert_eq!(cred["access"], "dt");
        assert_eq!(cred["deviceChat"], true);
        let host = if site == Site::QwenWork {
            "https://gateway.qwenwork.cn"
        } else {
            "https://acme.example.com"
        };
        assert_eq!(cred["accountIdentity"]["issuer"], host);
        assert_eq!(cred["accountIdentity"]["subject"], "u1");
        let requests = client.requests.lock().unwrap();
        assert_eq!(requests.len(), 2);
        assert!(requests.iter().all(|r| r.url.starts_with(host)));
        assert!(requests[1].url.contains(
            "/api/v1/adapter/user/account-context?include=user,plan,quota,page,data_sharing"
        ));
        assert_eq!(requests[1].headers["authorization"], "Bearer dt");
    }
}
#[tokio::test]
async fn enterprise_domain_rejects_non_public_addresses_before_network_and_never_changes_hosts() {
    let client = Client::new(vec![]);
    let provider = configured(Site::QwenWorkEnterprise, client.clone());
    for domain in [
        "localhost",
        "127.0.0.1",
        "[::1]",
        "acme.local",
        "acme.internal",
        "http://acme.example.com",
        "https://acme.example.com:8443",
        "https://user:pass@acme.example.com",
        "https://acme.example.com/path",
        "https://acme.example.com?x=1",
        "https://acme.example.com#x",
        "bad-.example.com",
    ] {
        assert!(
            auth::authorize(&provider, json!({"inputs":{"domain":domain}}))
                .await
                .is_err(),
            "{domain}"
        );
    }
    for cred in [
        credentials(),
        json!({"enterpriseHost":"acme.example.com","vpc":"different.example.com"}),
        json!({"enterpriseHost":"http://acme.example.com"}),
    ] {
        assert!(provider.deployment(&cred).is_err());
    }
    assert!(client.requests.lock().unwrap().is_empty());
}
#[tokio::test]
async fn qwork_catalog_and_chat_use_real_site_headers_and_body() {
    for enterprise in [false, true] {
        let site = if enterprise {
            Site::QwenWorkEnterprise
        } else {
            Site::QwenWork
        };
        let client = Client::new(vec![(200, catalog()), (200, answer())]);
        let provider = configured(site, client.clone());
        let host = if enterprise {
            "https://acme.example.com"
        } else {
            "https://gateway.qwenwork.cn"
        };
        let request:FetchRequest=serde_json::from_value(json!({"account":"one","credentials":account(enterprise),"method":"POST","url":format!("{host}/v1/chat/completions"),"body":base64::engine::general_purpose::STANDARD.encode(json!({"model":"flash","messages":[{"role":"user","content":"hello"}]}).to_string())})).unwrap();
        let bytes = collect(provider.fetch(request).await.unwrap()).await;
        assert!(String::from_utf8(bytes).unwrap().contains("hello"));
        let requests = client.requests.lock().unwrap();
        assert_eq!(requests.len(), 2);
        for request in requests.iter() {
            assert!(request.url.starts_with(host));
            assert_eq!(request.headers["cosy-clienttype"], "6");
            assert_eq!(request.headers["cosy-scene"], "qwork");
            assert_eq!(request.headers["cosy-business-product"], "qoder_work");
            assert_eq!(request.headers["cosy-version"], "1.1.59");
        }
        let body: Value = serde_json::from_str(
            &codec::decode(std::str::from_utf8(requests[1].body.as_ref().unwrap()).unwrap())
                .unwrap(),
        )
        .unwrap();
        assert_eq!(body["session_type"], "qoder_work");
        assert_eq!(body["business"]["product"], "qoder_work");
        assert_eq!(body["business"]["version"], "1.1.59");
        assert!(text(&body["system"][0], "text").contains("You are QwenWork"));
        assert!(!text(&body["system"][0], "text").contains("Qoder"));
    }
}
#[tokio::test]
async fn only_enterprise_503_falls_back_to_the_shipped_catalog() {
    for (site, status, success) in [
        (Site::QwenWorkEnterprise, 503, true),
        (Site::QwenWorkEnterprise, 500, false),
        (Site::QwenWork, 503, false),
    ] {
        let client = Client::new(vec![(status, json!({"message":"Unavailable"}))]);
        let provider = configured(site, client.clone());
        let result = provider
            .listing(
                client.as_ref(),
                &account(site == Site::QwenWorkEnterprise),
                true,
            )
            .await;
        if success {
            let entries = result.unwrap();
            assert_eq!(
                entries.iter().map(|e| e.key.as_str()).collect::<Vec<_>>(),
                ["flash", "pro", "qwen3.8-max-preview"]
            );
            assert_eq!(entries[0].context, 131072);
        } else {
            assert!(result.is_err());
        }
    }
}
#[tokio::test]
async fn credits_read_only_the_accounts_own_service_and_personal_wallets() {
    for (site, biz, wallet) in [
        (Site::QwenWork, false, true),
        (Site::QwenWork, true, false),
        (Site::QwenWorkEnterprise, false, false),
    ] {
        let mut replies = vec![(
            200,
            json!({"data":{"user":{"is_biz":biz},"plan":{"name":"Plan"},"quota":{"remaining":60}}}),
        )];
        if wallet {
            replies.push((200, json!({"daily_credits":{"total_balance":5}})));
        }
        let client = Client::new(replies);
        let provider = configured(site, client.clone());
        let value = work_usage::fetch(
            &provider,
            client.as_ref(),
            account(site == Site::QwenWorkEnterprise),
        )
        .await
        .unwrap();
        assert_eq!(value["balances"][0]["amount"], 60.0);
        assert_eq!(value["plan"], "Plan");
        assert!(value["items"].as_array().unwrap().is_empty());
        let requests = client.requests.lock().unwrap();
        assert_eq!(requests.len(), if wallet { 2 } else { 1 });
        assert_eq!(requests[0].headers["authorization"], "Bearer device-one");
        assert!(requests
            .iter()
            .all(|r| r.url.starts_with(if site == Site::QwenWorkEnterprise {
                "https://acme.example.com/"
            } else {
                "https://gateway.qwenwork.cn/"
            })));
    }
}
#[tokio::test]
async fn refused_device_tokens_rotate_once_with_target_c_before_retrying_usage() {
    let client = Client::new(vec![
        (401, json!({})),
        (
            200,
            json!({"token":"new","refresh_token":"new-refresh","expires_in":3600}),
        ),
        (200, json!({"quota":{"total":100,"remaining":60}})),
    ]);
    let provider = configured(Site::QwenWorkEnterprise, client.clone());
    let (value, saved) = rpc(provider, "usage", json!({"credentials":account(true)})).await;
    assert_eq!(value["result"]["items"][0]["usedPercent"], 40.0);
    assert_eq!(saved.len(), 1);
    assert_eq!(saved[0]["access"], "new");
    let requests = client.requests.lock().unwrap();
    assert_eq!(requests.len(), 3);
    assert!(requests[1].url.contains("/api/v1/deviceToken/refresh"));
    let body: Value = serde_json::from_slice(requests[1].body.as_ref().unwrap()).unwrap();
    assert_eq!(body["target"], "c");
    assert_eq!(requests[2].headers["authorization"], "Bearer new");
    assert!(requests
        .iter()
        .all(|r| r.url.starts_with("https://acme.example.com/")));
}
#[tokio::test]
async fn work_sites_do_not_offer_personal_tokens_or_campaign_actions() {
    for site in [Site::QwenWork, Site::QwenWorkEnterprise] {
        let provider = configured(site, Client::new(vec![]));
        assert!(auth::authorize(
            &provider,
            json!({"authMethodID":"pat","inputs":{"pat":"pt-token"}})
        )
        .await
        .is_err());
        assert!(provider.call("checkin", json!({})).await.is_err());
    }
}

async fn rpc(provider: Qoder, method: &str, params: Value) -> (Value, Vec<Value>) {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    let site = provider.site;
    let manifest: Value = serde_json::from_str(if site == Site::QwenWorkEnterprise {
        include_str!("../../qwenwork-enterprise/nora-provider.json")
    } else {
        include_str!("../../qwenwork/nora-provider.json")
    })
    .unwrap();
    let (host, plugin) = tokio::io::duplex(65536);
    let (read, write) = tokio::io::split(plugin);
    let task = tokio::spawn(nora_provider_plugin_sdk::serve_io(
        Arc::new(provider),
        read,
        write,
    ));
    let (read, mut write) = tokio::io::split(host);
    let mut read = BufReader::new(read);
    let mut line = String::new();
    write.write_all(format!("{}\n",json!({"id":1,"method":"initialize","params":{"apiVersion":1,"pluginID":site.id(),"pluginDirectory":"/test/plugin","dataDirectory":"/test/data","manifest":manifest,"hostCapabilities":{"credentialUpdates":true}}})).as_bytes()).await.unwrap();
    read.read_line(&mut line).await.unwrap();
    assert!(serde_json::from_str::<Value>(&line)
        .unwrap()
        .get("result")
        .is_some());
    write
        .write_all(format!("{}\n", json!({"id":2,"method":method,"params":params})).as_bytes())
        .await
        .unwrap();
    let mut saved = Vec::new();
    let result = loop {
        line.clear();
        tokio::time::timeout(Duration::from_secs(3), read.read_line(&mut line))
            .await
            .unwrap()
            .unwrap();
        assert!(!line.is_empty());
        let value: Value = serde_json::from_str(&line).unwrap();
        if value["event"] == "credentials" {
            saved.push(value["credentials"].clone());
            write.write_all(format!("{}\n",json!({"id":0,"method":"credentialsSaved","params":{"id":2,"updateID":value["updateID"],"success":true}})).as_bytes()).await.unwrap();
        } else if value["id"] == 2 {
            break value;
        }
    };
    task.abort();
    (result, saved)
}
