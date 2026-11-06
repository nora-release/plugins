use super::*;

#[test]
fn xml_samples_survive_all_character_chunks_and_all_two_way_splits() {
    for (input, expected) in [
        (
            ARRAY_CALL.to_owned(),
            vec![json!(["compress", {"content":[{"summary":"TASK（历史）","endId":"m03680"}]}])],
        ),
        (CALLS.to_owned(), expected_calls()),
        (
            CALLS
                .replace("<function_calls>", "<antml:function_calls>")
                .replace("</function_calls>", "</antml:function_calls>")
                .replace("<invoke", "<antml:invoke")
                .replace("</invoke>", "</antml:invoke>")
                .replace("<parameter", "<antml:parameter")
                .replace("</parameter>", "</antml:parameter>"),
            expected_calls(),
        ),
    ] {
        let text = format!("处理中。\n{input}\n");
        let expected = Observed {
            text: "处理中。\n".into(),
            calls: expected,
            error: None,
        };
        for size in [0, 1, 2, 3, 5, 7, 13, 64] {
            assert_eq!(
                read(&pieces(&text, size), tools()),
                expected,
                "chunk {size}"
            );
        }
        for (index, _) in text.char_indices().skip(1) {
            assert_eq!(
                read(&[text[..index].into(), text[index..].into()], tools()),
                expected,
                "split {index}"
            );
        }
    }
}

#[test]
fn xml_parameter_schema_whitespace_escapes_and_array_values_are_preserved() {
    for input in [
        STRING_CALL.to_owned(),
        format!("<tool_call>{STRING_CALL}</tool_call>"),
    ] {
        for size in [0, 1, 5] {
            assert_eq!(
                read(&pieces(&input, size), tools()),
                Observed {
                    calls: vec![json!(["bash", {"command":"[1, 2]"}])],
                    ..Observed::default()
                }
            );
        }
    }
    let raw = "  &lt;x&gt;\\n \\\"中\\\" &amp;  ";
    let input = format!(
        "<invoke name=\"bash\"><parameter name=\"command\">\r\n{raw}\r\n</parameter></invoke>"
    );
    for size in [0, 1, 3] {
        assert_eq!(
            read(&pieces(&input, size), tools()),
            Observed {
                calls: vec![json!(["bash", {"command":raw}])],
                ..Observed::default()
            }
        );
    }
}

#[test]
fn xml_parameter_closes_allow_missing_invoke_closes_and_eof() {
    let input = "<function_calls>\n<invoke name=\"bash\">\n<parameter name=\"command\">ls</parameter>\n<invoke name=\"bash\">\n<parameter name=\"command\">pwd</parameter>\n</function_calls>";
    for size in [0, 1, 4] {
        assert_eq!(
            read(&pieces(input, size), tools()),
            Observed {
                calls: vec![
                    json!(["bash", {"command":"ls"}]),
                    json!(["bash", {"command":"pwd"}])
                ],
                ..Observed::default()
            }
        );
        assert_eq!(
            read(
                &pieces("<function=bash><parameter=command>ls</parameter>", size),
                tools()
            ),
            Observed {
                calls: vec![json!(["bash", {"command":"ls"}])],
                ..Observed::default()
            }
        );
    }
}

#[test]
fn xml_cutoff_reports_an_error_without_leaking_markup_or_losing_completed_calls() {
    for text in [
        CUT.to_owned(),
        "<function_calls><invoke name=\"bash\"><parameter name=\"command\">ls -".into(),
        "<function=compress><parameter=content>[{\"summary\":\"cut".into(),
        format!("{CUT}<function=bash><parameter=command>later</parameter></function>"),
    ] {
        for prefix in ["Working.\n".to_owned(), format!("Working.\n{STRING_CALL}")] {
            for size in [0, 1, 6] {
                let result = read(&pieces(&format!("{prefix}{text}"), size), tools());
                assert_eq!(result.text, "Working.\n");
                assert_eq!(
                    result.calls,
                    if prefix.contains(STRING_CALL) {
                        vec![json!(["bash", {"command":"[1, 2]"}])]
                    } else {
                        vec![]
                    }
                );
                assert!(
                    result
                        .error
                        .as_deref()
                        .is_some_and(|m| m.contains("cut short")),
                    "{result:?}"
                );
            }
        }
    }
}

#[test]
fn xml_prose_quoted_examples_and_undeclared_tools_are_never_calls() {
    for text in plain_text().into_iter().chain([CALLS.to_owned()]) {
        for size in [0, 1, 3, 8] {
            let declared = if text == CALLS { Vec::new() } else { tools() };
            assert_eq!(
                read(&pieces(&text, size), declared),
                Observed {
                    text: text.clone(),
                    ..Observed::default()
                }
            );
        }
    }
}

#[test]
fn xml_closing_tag_tolerance_matches_the_official_parser() {
    for input in [
        "<function=bash><parameter=command>ls</function>",
        "<invoke name=\"bash\"><parameter name=\"command\">ls</invoke>",
    ] {
        for size in [0, 1, 7] {
            assert_eq!(
                read(&pieces(input, size), tools()),
                Observed {
                    calls: vec![json!(["bash", {"command":"ls"}])],
                    ..Observed::default()
                }
            );
        }
    }
    let empty = read(&pieces("<function=bash></function>", 1), tools());
    assert_eq!(empty.calls, vec![json!(["bash", {}])]);
    assert!(empty.error.is_none());
}
