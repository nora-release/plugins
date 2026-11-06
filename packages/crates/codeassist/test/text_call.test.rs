use super::*;
#[test]
fn split_markers_and_arguments_become_calls_without_losing_surrounding_text() {
    let mut parser = TextCalls::default();
    let names = envelope::Names::default();
    let mut parts = parser.feed("Let's look. call:def", &names);
    assert_eq!(parts[0]["text"], "Let's look. ");
    parts.extend(parser.feed(
        "ault_api:bash{command:<ctrl46>echo {hello}<ctrl46>,count:2} done",
        &names,
    ));
    assert_eq!(parts[1]["functionCall"]["name"], "bash");
    assert_eq!(parts[1]["functionCall"]["args"]["command"], "echo {hello}");
    assert_eq!(parts[1]["functionCall"]["args"]["count"], 2);
    assert_eq!(parts[2]["text"], " done");
}
#[test]
fn ordinary_mentions_and_incomplete_calls_remain_text() {
    let mut parser = TextCalls::default();
    let names = envelope::Names::default();
    let parts = parser.feed("call:default_api:not a call", &names);
    assert_eq!(
        parts
            .iter()
            .map(|part| part["text"].as_str().unwrap())
            .collect::<String>(),
        "call:default_api:not a call"
    );
    assert!(parser.feed("call:default_api:tool{a:", &names).is_empty());
    assert_eq!(parser.flush()[0]["text"], "call:default_api:tool{a:");
}
#[test]
fn json_values_and_bare_values_are_both_supported() {
    assert_eq!(
        args("command:ls -la,path:/tmp,count:2"),
        Some(json!({"command":"ls -la","path":"/tmp","count":2}))
    );
    assert_eq!(
        args("values:[1,2],nested:{\"x\":true}"),
        Some(json!({"values":[1,2],"nested":{"x":true}}))
    );
}
