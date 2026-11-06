use super::*;
#[test]
fn cli_errors_are_classified_without_echoing_private_account_contents() {
    for (value, expected) in [
        (
            json!({"api_error_status":429,"result":"private-token"}),
            429,
        ),
        (
            json!({"result":"OAuth session expired private-refresh"}),
            401,
        ),
        (json!({"result":"private-secret input too long"}), 400),
        (json!({"result":"private-token overloaded"}), 503),
    ] {
        let code = status(&value);
        assert_eq!(code, expected);
        assert_eq!(failure(code).status, expected);
    }
}
