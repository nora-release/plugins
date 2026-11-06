use super::*;
#[test]
fn account_get_matches_the_upstream_mcode_bundle_vector() {
    let site = Site::for_id("minimax-code").unwrap();
    let request = signed(
        site,
        "/v1/api/user/info",
        "tok",
        "",
        None,
        site.agent,
        1790000000123,
        28800,
        "darwin",
    )
    .unwrap();
    assert_eq!(request.url,"https://agent.minimaxi.com/v1/api/user/info?device_platform=mcode&biz_id=3&app_id=3001&version_code=22201&unix=1790000000123&timezone_offset=28800&sys_language=zh&lang=zh&device_id=0&os_name=darwin&browser_name=mcode&user_id=0&client=mcode");
    assert_eq!(request.headers["yy"], "ea384f50aae587ea2048557a2e1b30b1");
    assert_eq!(
        request.headers["x-signature"],
        "03015677c012162b07bc060014868ead"
    );
    assert!(request.body.is_none());
}
#[test]
fn account_post_matches_the_upstream_mcode_bundle_vector() {
    let site = Site::for_id("minimax-code-global").unwrap();
    let request = signed(
        site,
        "/matrix/api/v1/commerce/get_membership_info",
        "tok",
        "u-42",
        Some(&serde_json::json!({"workspace_id":7})),
        site.agent,
        1790000000123,
        0,
        "linux",
    )
    .unwrap();
    assert_eq!(request.headers["yy"], "078fc5529cd0bee0e3f4b587421d726d");
    assert_eq!(
        request.headers["x-signature"],
        "cdc435f7a98e23c841ecd445dfa82439"
    );
    assert_eq!(request.body, Some(b"{\"workspace_id\":7}".to_vec()));
}
