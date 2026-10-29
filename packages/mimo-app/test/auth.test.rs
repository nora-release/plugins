use super::*;
use crate::test_support::{raw, reply, Fixture};
fn creds() -> Value {
    json!({"userId":"one","passToken":"private-pass","cUserId":"cipher-id","deviceId":"pc_device","region":"SGP"})
}
fn me(expected: &str, uid: &str, region: &str, token: &str) -> crate::test_support::Response {
    reply(
        expected,
        200,
        json!({"code":0,"data":{"userId":uid,"region":region,"nickname":"Name"}}),
    )
    .cookie(&format!("serviceToken={token}; Path=/; Secure; HttpOnly"))
    .cookie(&format!("userId={uid}; Path=/; Secure"))
}
#[test]
fn xiaomi_json_guard_and_region_ids_are_real_vendor_values() {
    assert_eq!(
        account_json(b" &&&START&&&{\"code\":0,\"userId\":123}").unwrap()["userId"],
        123
    );
    assert_eq!(id(&json!(123)).as_deref(), Some("123"));
    assert_eq!(
        base("ru"),
        Some("https://mimo-server-ru.xiaomimimo.com/api")
    );
    assert!(base("unsupported").is_none());
    assert!(account_url("https://evil.example/poll").is_err());
    assert!(redirect(
        "https://account.xiaomi.com/login",
        "http://account.xiaomi.com/other",
        PLATFORM
    )
    .is_err());
}
#[tokio::test]
async fn session_follows_the_actual_sso_redirects_and_preserves_every_session_cookie() {
    let base = base("SGP").unwrap();
    let fixture = Fixture::new(vec![
        raw("/user/xiaomi/me", 302, Vec::new())
            .header("location", "https://account.xiaomi.com/pass/serviceLogin"),
        raw("account.xiaomi.com/pass/serviceLogin", 302, Vec::new())
            .header("location", "http://mimo-server-sgp.xiaomimimo.com/api/sts"),
        raw("/api/sts", 302, Vec::new())
            .header("location", "/api/user/xiaomi/me")
            .cookie("serviceToken=real-session; Path=/; Secure")
            .cookie("userId=one; Path=/; Secure"),
        reply(
            "/user/xiaomi/me",
            200,
            json!({"code":0,"data":{"userId":"one","region":"SGP"}}),
        ),
    ]);
    let session = session(fixture.as_ref(), &creds(), "SGP", None)
        .await
        .unwrap();
    assert_eq!(session.cookies["serviceToken"], "real-session");
    assert_eq!(session.cookies["userId"], "one");
    let requests = fixture.requests.lock().unwrap();
    assert_eq!(requests.len(), 4);
    assert!(requests[1].request.headers["cookie"].contains("passToken=private-pass"));
    assert!(requests[2].request.url.starts_with("https://"));
    assert!(!requests[2]
        .request
        .headers
        .values()
        .any(|value| value.contains("private-pass")));
    assert_eq!(requests[3].request.url, format!("{base}/user/xiaomi/me"));
}
#[tokio::test]
async fn redirect_boundaries_refuse_an_external_host_before_any_credential_can_be_sent() {
    let fixture =
        Fixture::new(vec![raw("/user/xiaomi/me", 302, Vec::new())
            .header("location", "https://evil.example/callback")]);
    assert!(session(fixture.as_ref(), &creds(), "SGP", None)
        .await
        .is_err());
    assert_eq!(fixture.requests.lock().unwrap().len(), 1);
}
#[tokio::test]
async fn sessions_distinguish_unsupported_regions_expired_signin_and_an_account_switch() {
    for (status, body, expected) in [
        (403, json!({"code":46109}), "upstream_transport_error"),
        (
            200,
            json!({"code":0,"data":{"userId":"other"}}),
            "authentication_required",
        ),
    ] {
        let fixture = Fixture::new(vec![
            reply("/user/xiaomi/me", status, body).cookie("serviceToken=some; Path=/; Secure")
        ]);
        assert_eq!(
            session(fixture.as_ref(), &creds(), "SGP", None)
                .await
                .err()
                .unwrap()
                .code,
            expected
        );
    }
    let fixture = Fixture::new(vec![raw(
        "/user/xiaomi/me",
        200,
        b"<html>sign in</html>".to_vec(),
    )]);
    assert_eq!(
        session(fixture.as_ref(), &creds(), "SGP", None)
            .await
            .err()
            .unwrap()
            .code,
        "authentication_required"
    );
}
#[tokio::test]
async fn qr_ticket_uses_the_servers_sid_and_poll_page_and_never_invents_login_success() {
    let fixture=Fixture::new(vec![raw("/user/xiaomi/me",302,Vec::new()).header("location","https://account.xiaomi.com/pass/serviceLogin?sid=actual-sid&callback=https%3A%2F%2Fmimo-server-sgp.xiaomimimo.com%2Fapi%2Fsts"),raw("/longPolling/loginUrl?",200,b"&&&START&&&{\"code\":0,\"loginUrl\":\"https://account.xiaomi.com/qr\",\"lp\":\"https://account.xiaomi.com/poll/ticket\",\"timeout\":60}".to_vec()),raw("/poll/ticket",200,b"&&&START&&&{\"userId\":\"one\",\"passToken\":\"private-pass\",\"cUserId\":\"cipher\"}".to_vec())]);
    let ticket = login(fixture.as_ref(), None).await.unwrap();
    assert!(ticket.device.starts_with("pc_"));
    assert!(ticket.until <= core::now() + 60_000);
    let result = poll(
        fixture.as_ref(),
        &json!({"until":ticket.until,"poll":ticket.poll,"device":ticket.device}),
        None,
    )
    .await
    .unwrap();
    assert_eq!(result["passToken"], "private-pass");
    let requests = fixture.requests.lock().unwrap();
    let url = url::Url::parse(&requests[1].request.url).unwrap();
    assert_eq!(
        url.query_pairs().find(|(key, _)| key == "sid").unwrap().1,
        "actual-sid"
    );
}
#[tokio::test]
async fn account_region_switch_reauthenticates_at_the_real_regional_server() {
    let fixture = Fixture::new(vec![
        me(
            "mimo-server-sgp.xiaomimimo.com/api/user/xiaomi/me",
            "one",
            "RU",
            "sgp-session",
        ),
        me(
            "mimo-server-ru.xiaomimimo.com/api/user/xiaomi/me",
            "one",
            "RU",
            "ru-session",
        ),
    ]);
    let value = signed_in(
        fixture.as_ref(),
        &json!({"userId":"one","passToken":"private-pass"}),
        "pc_device",
        None,
    )
    .await
    .unwrap();
    assert_eq!(value["xiaomi"]["region"], "RU");
    assert_eq!(value["cookies"]["serviceToken"], "ru-session");
    assert_eq!(value["uid"], "one");
    assert_eq!(fixture.requests.lock().unwrap().len(), 2);
}
#[tokio::test]
async fn platform_signin_keeps_account_cookies_off_the_platform_and_reads_only_the_requested_page()
{
    let fixture = Fixture::new(vec![
        reply(
            "/api/v1/tokenPlan/detail",
            401,
            json!({"loginUrl":"https://account.xiaomi.com/pass/serviceLogin?sid=api-platform"}),
        ),
        raw("account.xiaomi.com/pass/serviceLogin", 302, Vec::new())
            .header("location", "https://platform.xiaomimimo.com/sts"),
        raw("platform.xiaomimimo.com/sts", 302, Vec::new())
            .header("location", "/api/v1/tokenPlan/detail")
            .cookie("serviceToken=platform-session; Path=/; Secure")
            .cookie("userId=one; Path=/; Secure"),
        reply(
            "/api/v1/tokenPlan/detail",
            200,
            json!({"code":0,"data":{"planName":"Pro","expired":false}}),
        ),
    ]);
    let (cookies, value) =
        platform_page(fixture.as_ref(), &creds(), "/tokenPlan/detail", None, None)
            .await
            .unwrap();
    assert_eq!(cookies["serviceToken"], "platform-session");
    assert_eq!(value["planName"], "Pro");
    let requests = fixture.requests.lock().unwrap();
    assert!(requests[1].request.headers["cookie"].contains("passToken=private-pass"));
    assert!(!requests[2]
        .request
        .headers
        .values()
        .any(|value| value.contains("private-pass")));
}
