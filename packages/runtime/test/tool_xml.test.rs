use super::*;

fn read(text: &str, end: bool) -> Parsed<'_> {
    parse(
        text,
        end,
        |name| (name == "bash").then(|| name.into()),
        |_, key| key == "command",
    )
}

#[test]
fn xml_opener_and_partial_marker_locations_are_byte_offsets() {
    assert_eq!(at("中文 <invoke name="), Some("中文 ".len()));
    assert_eq!(hold_at("中文 <antml:invoke na"), Some("中文 ".len()));
    assert_eq!(at("`<function=bash>`"), Some(1));
    assert_eq!(
        opener_len("<invoke name=\"bash\">tail"),
        "<invoke name=\"bash\">".len()
    );
    assert_eq!(
        leftover_len(" \n</invoke></function_calls>text"),
        " \n</invoke></function_calls>".len()
    );
}

#[test]
fn xml_parser_distinguishes_wait_text_and_truncated_known_calls() {
    for input in [
        "<function_calls>",
        "<invoke name=\"ba",
        "<function=bash><parameter=command>ls",
    ] {
        assert_eq!(read(input, false), Parsed::Wait, "{input}");
    }
    assert_eq!(
        read("<function=bash><parameter=command>ls", true),
        Parsed::CutShort
    );
    assert_eq!(read("<invoke name=\"bash\"> prose", true), Parsed::Text);
    assert_eq!(
        read("<function=nope><parameter=command>ls", true),
        Parsed::Text
    );
    assert_eq!(read("<function_calls>", true), Parsed::Text);
}

#[test]
fn xml_wire_escaping_is_literal_and_only_outer_newlines_are_removed() {
    let input = "<antml:invoke name=\"bash\"><antml:parameter name=\"command\">\r\n  &lt;x&gt;\\n &amp; \\\"中\\\"  \r\n</antml:parameter></antml:invoke>tail";
    assert_eq!(
        read(input, true),
        Parsed::Call {
            name: "bash".into(),
            args: serde_json::json!({"command":"  &lt;x&gt;\\n &amp; \\\"中\\\"  "}),
            rest: "tail"
        }
    );
}

#[test]
fn xml_complete_parameters_can_end_at_the_next_invoke_or_eof() {
    let next = "<invoke name=\"bash\"><parameter name=\"command\">two</parameter></invoke>";
    let prefix = "<invoke name=\"bash\"><parameter name=\"command\">one</parameter>";
    for end in [true, false] {
        assert_eq!(
            read(&format!("{prefix}{next}"), end),
            Parsed::Call {
                name: "bash".into(),
                args: serde_json::json!({"command":"one"}),
                rest: next
            }
        );
    }
    assert_eq!(
        read(prefix, true),
        Parsed::Call {
            name: "bash".into(),
            args: serde_json::json!({"command":"one"}),
            rest: ""
        }
    );
}

#[test]
fn xml_matches_official_tolerance_for_explicit_closing_tags() {
    assert_eq!(
        read("<function=bash><parameter=command>ls</function>", true),
        Parsed::Call {
            name: "bash".into(),
            args: serde_json::json!({"command":"ls"}),
            rest: ""
        }
    );
    assert_eq!(
        read("<function=bash></function>", true),
        Parsed::Call {
            name: "bash".into(),
            args: serde_json::json!({}),
            rest: ""
        }
    );
    assert_eq!(value("[1, 2]", true), serde_json::json!("[1, 2]"));
    assert_eq!(value("[1, 2]", false), serde_json::json!([1, 2]));
    assert_eq!(value("{invalid", false), serde_json::json!("{invalid"));
}
