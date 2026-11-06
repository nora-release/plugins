use super::*;

struct ResponseClient {
    status: u16,
}
#[async_trait]
impl HttpClient for ResponseClient {
    async fn stream(&self, _: HttpRequest) -> Result<HttpStreamingResponse, HttpClientError> {
        unreachable!()
    }
    async fn request(&self, _: HttpRequest) -> Result<HttpResponse, HttpClientError> {
        if self.status == 0 {
            return Err(HttpClientError::Transport(
                "private-network-diagnostic".into(),
            ));
        }
        Ok(HttpResponse {
            status: self.status,
            headers: BTreeMap::new(),
            body: json!({"error":"invalid_grant"}).to_string().into_bytes(),
        })
    }
}

#[tokio::test]
async fn source_refresh_distinguishes_vendor_refusal_from_temporary_failures_without_erasing_codex()
{
    for status in [400, 401, 403, 429, 503, 0, 200] {
        let directory = Directory::new();
        let path = directory.auth_file(codex_file(
            &user_access("person-a", "workspace", 3600),
            "workspace",
        ));
        let credentials = bind(&path).unwrap();
        let original = std::fs::read(&path).unwrap();
        let error = resolve(&credentials, true, &ResponseClient { status })
            .await
            .unwrap_err();
        assert_eq!(
            error,
            if matches!(status, 400 | 401) {
                SourceError::RefreshRefused
            } else {
                SourceError::RefreshFailed
            }
        );
        assert_eq!(std::fs::read(&path).unwrap(), original);
        assert!(credentials.access_token.is_empty());
        assert!(credentials.refresh_token.is_none());
        if !matches!(status, 400 | 401) {
            assert!(!error.message().to_lowercase().contains("sign in"));
        }
        let no_refresh = client(json!({}));
        assert!(resolve(&credentials, false, &no_refresh).await.is_ok());
        assert_eq!(no_refresh.requests.load(Ordering::Relaxed), 0);
    }
}

#[tokio::test]
async fn a_source_can_retry_or_follow_a_new_sign_in_after_a_failed_renewal() {
    for status in [400, 429] {
        let directory = Directory::new();
        let path = directory.auth_file(codex_file(
            &user_access("person-a", "workspace", -60),
            "workspace",
        ));
        let credentials = bind(&path).unwrap();
        assert!(resolve(&credentials, false, &ResponseClient { status })
            .await
            .is_err());
        if status == 400 {
            directory.auth_file(codex_file(
                &user_access("person-a", "workspace", 3600),
                "workspace",
            ));
        }
        let fresh = user_access("person-a", "workspace", 7200);
        let client = client(
            json!({"access_token":fresh,"refresh_token":"private-rotated","expires_in":7200}),
        );
        assert!(resolve(&credentials, false, &client).await.is_ok());
        assert_eq!(
            client.requests.load(Ordering::Relaxed),
            usize::from(status != 400)
        );
    }
}

#[tokio::test]
async fn concurrent_source_refreshes_do_not_spend_the_same_rotation_twice() {
    struct DelayedClient {
        requests: AtomicUsize,
        entered: tokio::sync::Notify,
        release: tokio::sync::Notify,
        body: Value,
    }
    #[async_trait]
    impl HttpClient for DelayedClient {
        async fn stream(&self, _: HttpRequest) -> Result<HttpStreamingResponse, HttpClientError> {
            unreachable!()
        }
        async fn request(&self, _: HttpRequest) -> Result<HttpResponse, HttpClientError> {
            assert_eq!(self.requests.fetch_add(1, Ordering::Relaxed), 0);
            self.entered.notify_one();
            self.release.notified().await;
            Ok(HttpResponse {
                status: 200,
                headers: BTreeMap::new(),
                body: self.body.to_string().into_bytes(),
            })
        }
    }
    let directory = Directory::new();
    let path = directory.auth_file(codex_file(
        &user_access("person-a", "workspace", -60),
        "workspace",
    ));
    let credentials = bind(&path).unwrap();
    let fresh = user_access("person-a", "workspace", 7200);
    let client = Arc::new(DelayedClient {
        requests: AtomicUsize::new(0),
        entered: Default::default(),
        release: Default::default(),
        body: json!({"access_token":fresh,"refresh_token":"private-rotated","expires_in":7200}),
    });
    let waiting_client = client.clone();
    let first_credentials = credentials.clone();
    let first =
        tokio::spawn(async move { resolve(&first_credentials, true, &*waiting_client).await });
    client.entered.notified().await;
    let waiting_client = client.clone();
    let second = tokio::spawn(async move { resolve(&credentials, true, &*waiting_client).await });
    tokio::task::yield_now().await;
    client.release.notify_one();
    for result in [first.await.unwrap(), second.await.unwrap()] {
        assert_eq!(result.unwrap().access_token, fresh);
    }
    assert_eq!(client.requests.load(Ordering::Relaxed), 1);
    let saved: Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    assert_eq!(saved["tokens"]["refresh_token"], "private-rotated");
}
