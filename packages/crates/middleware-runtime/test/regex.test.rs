use nora_gateway_middleware::regex::Expression;
#[test]
fn javascript_substitutions_and_named_groups() {
    let re = Expression::new("(?<word>a)(b)?", "").unwrap();
    assert_eq!(
        re.replace(
            "za!",
            "$$/$&/$`/$'/$1/$2/$01/$10/$<word>/$<missing>/$0",
            false
        ),
        "z$/a/z/!/a//a/a0/a//$0!"
    );
}
#[test]
fn lookaround_backreferences_and_case_insensitive_rules() {
    assert_eq!(
        Expression::new(r"(?<=x)(a)\1(?=z)", "i")
            .unwrap()
            .replace("xAAz / xabz", "ok", true),
        "xokz / xabz"
    );
    for flags in ["gg", "uv", "q"] {
        assert!(Expression::new("x", flags).is_err());
    }
}
#[test]
fn sticky_test_and_replace_share_the_ecmascript_cursor() {
    let mut re = Expression::new("a", "y").unwrap();
    assert_eq!(re.tested_replace("a", "b"), Some("a".into()));
    assert_eq!(re.tested_replace("aa", "b"), Some("ab".into()));
    assert_eq!(re.tested_replace("abc", "b"), None);
}
#[test]
fn words_use_javascript_whitespace_trimming() {
    let re = nora_gateway_middleware::regex::matcher(
        &serde_json::json!({"words":["\u{feff}secret\u{feff}"]}),
        false,
    )
    .unwrap()
    .unwrap();
    assert_eq!(re.first("SECRET"), Some("SECRET"));
}
