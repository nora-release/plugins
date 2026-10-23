use super::*;
fn tools() -> Vec<Value> {
    json!([{"type":"function","function":{"name":"bash","parameters":{"type":"object","properties":{"command":{"type":"string"},"description":{"type":"string"},"workdir":{"type":"string"}}}}},{"type":"function","function":{"name":"Write","parameters":{"type":"object","properties":{"content":{"type":"string"},"path":{"type":"string"}}}}},{"type":"function","function":{"name":"read","parameters":{"type":"object","properties":{"file_path":{"type":"string"},"limit":{"type":"number"},"offset":{"type":"number"}}}}},{"type":"function","function":{"name":"task","parameters":{"type":"object","properties":{"input":{"type":"object"}}}}}]).as_array().unwrap().clone()
}
fn read_with(input: &str, size: usize) -> PluginResult<TextParts> {
    let mut t = TextTools::new(tools());
    let chars = input.chars().collect::<Vec<_>>();
    let mut out = TextParts::default();
    for piece in chars.chunks(if size == 0 { chars.len().max(1) } else { size }) {
        let r = t.push(&piece.iter().collect::<String>(), false)?;
        out.text.push_str(&r.text);
        out.reasoning.push_str(&r.reasoning);
        out.calls.extend(r.calls);
    }
    let r = t.push("", true)?;
    out.text.push_str(&r.text);
    out.reasoning.push_str(&r.reasoning);
    out.calls.extend(r.calls);
    Ok(out)
}
fn args(call: &ToolCall) -> Value {
    serde_json::from_str(&call.arguments).unwrap()
}
fn cases(input: &str, expected: Value, text: &str, name: &str) {
    for size in [0, 1, 3, 7] {
        let value = read_with(input, size).unwrap();
        assert_eq!(value.text, text, "size {size}");
        assert_eq!(value.calls.len(), 1, "size {size}");
        assert_eq!(value.calls[0].name, name);
        assert_eq!(args(&value.calls[0]), expected);
        assert!(value.calls[0].id.starts_with("call_"));
    }
}
#[test]
fn standard_json_calls_survive_every_boundary_without_leaking_markup() {
    cases(
        "答复。\n<tool_call>{\"name\":\"bash\",\"arguments\":{\"command\":\"ls\"}}</tool_call>",
        json!({"command":"ls"}),
        "答复。\n",
        "bash",
    );
}
#[test]
fn whole_json_at_eof_needs_no_synthetic_closing_tag() {
    cases("答复。\n<tool_call>{\"name\":\"write\",\"arguments\":{\"content\":\"P\\n\\n\",\"path\":\"a\"}}",json!({"content":"P\n\n","path":"a"}),"答复。\n","Write");
}
#[test]
fn no_underscore_markup_and_empty_blocks_are_handled() {
    cases(
        "<toolcall>{\"name\":\"bash\",\"arguments\":{\"command\":\"ls\"}}</toolcall>",
        json!({"command":"ls"}),
        "",
        "bash",
    );
    for size in [0, 1, 4] {
        let value = read_with("<toolcall></toolcall>", size).unwrap();
        assert!(value.calls.is_empty());
        assert!(value.text.is_empty());
    }
}
#[test]
fn attribute_and_parameter_forms_keep_declared_string_types() {
    cases(
        "<tool_call name=\"bash\">\n<parameter name=\"command\">123</parameter>\n</tool_call>",
        json!({"command":"123"}),
        "",
        "bash",
    );
    cases("<tool_call>{\"name\":\"read\"}>\n<parameter=file_path>/a</parameter><parameter=limit>55</parameter></tool_call>",json!({"file_path":"/a","limit":55}),"","read");
}
#[test]
fn glm_template_keeps_string_and_numeric_parameters() {
    cases("<tool_call>read<arg_key>file_path</arg_key><arg_value>123</arg_value><arg_key>limit</arg_key><arg_value>55</arg_value></tool_call>",json!({"file_path":"123","limit":55}),"","read");
}
#[test]
fn named_glm_template_is_a_real_call() {
    cases(
        "<tool_call name=\"bash\"><arg_key>command</arg_key><arg_value>ls</arg_value></tool_call>",
        json!({"command":"ls"}),
        "",
        "bash",
    );
}
#[test]
fn escaped_whole_json_and_raw_controls_are_repaired_losslessly() {
    cases(
        r#"<tool_call>{\"name\":\"bash\",\"arguments\":{\"command\":\"ls\"}}</tool_call>"#,
        json!({"command":"ls"}),
        "",
        "bash",
    );
    cases(
        "<tool_call>{\"name\":\"Write\",\"arguments\":{\"content\":\"a\nb\t中\"}}</tool_call>",
        json!({"content":"a\nb\t中"}),
        "",
        "Write",
    );
}
#[test]
fn invalid_backslashes_and_unescaped_quotes_keep_file_content() {
    for content in [
        "他说\"官方\"资料",
        r"snake\_case 和 \*强调\*",
        r"路径 C:\Users\arno\x",
    ] {
        let input = format!(
            r#"<tool_call>{{"name":"Write","arguments":{{"content":"{content}"}}}}</tool_call>"#
        );
        cases(&input, json!({"content":content}), "", "Write");
    }
}
#[test]
fn double_argument_wrappers_are_removed_only_when_not_a_tool_parameter() {
    cases(
        r#"<tool_call>{"name":"write","arguments":{"arguments":{"input":{"content":"P\n\n","path":"a"}}}}</tool_call>"#,
        json!({"content":"P\n\n","path":"a"}),
        "",
        "Write",
    );
    let call = normalize_call(
        ToolCall {
            id: "id".into(),
            name: "task".into(),
            arguments: r#"{"input":{"q":1}}"#.into(),
        },
        &tools(),
    )
    .unwrap();
    assert_eq!(args(&call), json!({"input":{"q":1}}));
}
#[test]
fn aliases_keep_exact_caller_names_and_refuse_ambiguous_matches() {
    let t = json!([{"function":{"name":"list_files"}},{"function":{"name":"list-files"}}])
        .as_array()
        .unwrap()
        .clone();
    assert_eq!(tool_named("list_files", &t).unwrap(), "list_files");
    assert!(tool_named("ListFiles", &t).is_err());
    assert_eq!(tool_named("unknown", &tools()).unwrap(), "unknown");
}
#[test]
fn inline_code_and_fenced_examples_never_execute() {
    for input in [
        r#"Write `<tool_call>{"name":"bash","arguments":{}}</tool_call>` to call."#,
        "Each call goes in a `<toolcall>` block.",
        "The tag is `<tool_call name",
        "Example:\n```\n<tool_call>{\"name\":\"bash\",\"arguments\":{}}</tool_call>\n```\nDone.",
        "The <tool_call> tag wraps a call; </tool_call> ends it.",
        "<tool_call>hello</tool_call> ok",
    ] {
        for size in [0, 1, 6] {
            let value = read_with(input, size).unwrap();
            assert_eq!(value.text, input);
            assert!(value.calls.is_empty());
        }
    }
}
#[test]
fn tool_opener_or_parameters_cut_short_are_errors() {
    for cut in [
        "<tool_call name",
        "<tool_call",
        "<toolcall name=\"ba",
        r#"<tool_call>{"name":"bash","arguments":{"command":"ls"}"#,
        "<tool_call name=\"bash\"><parameter name=\"command\">ls -",
    ] {
        for size in [0, 1, 4] {
            assert!(read_with(cut, size).is_err(), "{cut} at size {size}");
        }
    }
}
#[test]
fn malformed_closed_call_never_becomes_plain_answer() {
    for size in [0, 1, 6] {
        assert!(read_with(
            r#"<tool_call>{"name":"bash","arguments":{"command":}}</tool_call>"#,
            size
        )
        .is_err());
    }
}
#[test]
fn glm_tails_are_dropped_with_their_call_and_as_native_leftovers() {
    let call = r#"<tool_call>{"name":"bash","arguments":{"command":"ls"}}"#;
    for tail in [
        "]</tool_call>",
        "</tool_call>",
        "}</tool_call>",
        r#"]}]}}"}}}}</tool_call>"#,
        "}}</tool_call>",
        "\"></tool_call>",
    ] {
        for size in [0, 1, 3, 7] {
            let value = read_with(&format!("Listing it.\n{call}{tail}"), size).unwrap();
            assert_eq!(value.text, "Listing it.\n");
            assert_eq!(value.calls.len(), 1);
            let value = read_with(tail, size).unwrap();
            assert_eq!(value.text, "");
            assert!(value.calls.is_empty());
        }
    }
}
#[test]
fn braces_quotes_and_closing_tags_in_prose_stay_text() {
    for input in [
        r#"The map is {"a": [1, {"b": 2}]}"#,
        r#"He said "done" >"#,
        "The </tool_call> tag ends a call.",
        "Call it like `{\"x\": 1}</tool_call>` in the prompt.",
    ] {
        for size in [0, 1, 5] {
            let value = read_with(input, size).unwrap();
            assert_eq!(value.text, input);
            assert!(value.calls.is_empty());
        }
    }
}
#[test]
fn trailing_non_call_garbage_before_close_refuses_guessing() {
    let input = r#"<tool_call>{"name":"bash","arguments":{"command":"ls"}}x
</tool_call>"#;
    for size in [0, 1, 6] {
        assert!(read_with(input, size).is_err());
    }
}
#[test]
fn parallel_dsml_calls_keep_names_ids_and_explicit_string_values() {
    let input = r#"<｜DSML｜function_calls><｜DSML｜invoke name="bash"><｜DSML｜parameter name="command" string="true">123</｜DSML｜parameter></｜DSML｜invoke><｜DSML｜invoke name="read"><｜DSML｜parameter name="limit">55</｜DSML｜parameter></｜DSML｜invoke></｜DSML｜function_calls>"#;
    for size in [0, 1, 5] {
        let value = read_with(input, size).unwrap();
        assert_eq!(value.text, "");
        assert_eq!(value.calls.len(), 2);
        assert_eq!(args(&value.calls[0]), json!({"command":"123"}));
        assert_eq!(args(&value.calls[1]), json!({"limit":55}));
        assert_ne!(value.calls[0].id, value.calls[1].id);
    }
}
#[test]
fn ascii_dsml_tags_work_and_truncated_invokes_fail() {
    cases(
        r#"<|DSML|invoke name="bash"><|DSML|parameter name="command" string="true">ls</|DSML|parameter></|DSML|invoke>"#,
        json!({"command":"ls"}),
        "",
        "bash",
    );
    for input in [
        r#"<|DSML|invoke name="bash"><|DSML|parameter name="command">ls"#,
        r#"<|DSML|invoke name="bash">"#,
    ] {
        assert!(read_with(input, 1).is_err());
    }
}
#[test]
fn quoted_dsml_examples_stay_prose() {
    let input = r#"`<|DSML|invoke name="bash"></|DSML|invoke>`"#;
    for size in [0, 1, 5] {
        let value = read_with(input, size).unwrap();
        assert_eq!(value.text, input);
        assert!(value.calls.is_empty());
    }
}
#[test]
fn reasoning_json_is_separate_from_answer_and_tools() {
    for size in [0, 1, 4] {
        let value = read_with("before {\"reasoning_content\":\"思考\\nnext\"}after", size).unwrap();
        assert_eq!(value.reasoning, "思考\nnext");
        assert_eq!(value.text, "before after");
    }
}
#[test]
fn ordinary_json_code_in_a_fence_is_not_reasoning() {
    let input = "```json\n{\"reasoning_content\":\"example\"}\n```";
    let value = read_with(input, 1).unwrap();
    assert_eq!(value.text, input);
    assert!(value.reasoning.is_empty());
}
#[test]
fn huge_integer_arguments_do_not_round() {
    cases(
        r#"<tool_call>{"name":"task","arguments":{"large":18446744073709551615}}</tool_call>"#,
        json!({"large":18446744073709551615u64}),
        "",
        "task",
    );
}
fn ev(id: &str, name: &str, args: Value) -> Value {
    json!({"id":id,"function":{"name":name,"arguments":args}})
}
fn native(events: &[Value]) -> PluginResult<Vec<ToolCall>> {
    let mut calls = NativeCalls::default();
    for event in events {
        calls.push(event)?;
    }
    calls.take(&tools())
}
#[test]
fn native_fragments_and_snapshots_assemble_once_under_original_id() {
    let value = native(&[
        ev("n1", "bash", json!("")),
        ev("n1", "", json!(r#"{"command":"ls"#)),
        ev("n1", "", json!(r#" -la"}"#)),
        ev("n1", "bash", json!(r#"{"command":"ls -la"}"#)),
    ])
    .unwrap();
    assert_eq!(value.len(), 1);
    assert_eq!(value[0].id, "n1");
    assert_eq!(args(&value[0]), json!({"command":"ls -la"}));
}
#[test]
fn native_parallel_ids_and_indexes_cannot_mix() {
    let value = native(&[
        json!({"index":0,"function":{"name":"bash","arguments":"{\"command\":"}}),
        json!({"index":1,"function":{"name":"read","arguments":"{\"limit\":"}}),
        json!({"index":0,"function":{"arguments":"\"ls\"}"}}),
        json!({"index":1,"function":{"arguments":"5}"}}),
    ])
    .unwrap();
    assert_eq!(value.len(), 2);
    assert_eq!(args(&value[0]), json!({"command":"ls"}));
    assert_eq!(args(&value[1]), json!({"limit":5}));
}
#[test]
fn native_wrapper_name_and_native_fields_merge_with_native_winning() {
    let name =
        r#"{"name":"bash","arguments":{"command</arg_key><arg_value>sed -n 1,9p a.ts</arg_value>"#;
    let values = native(&[ev(
        "c1",
        name,
        json!({"description":"Read a.ts","workdir":"/tmp"}),
    )])
    .unwrap();
    assert_eq!(values[0].id, "c1");
    assert_eq!(values[0].name, "bash");
    assert_eq!(
        args(&values[0]),
        json!({"command":"sed -n 1,9p a.ts","description":"Read a.ts","workdir":"/tmp"})
    );
}
#[test]
fn native_wrapper_fields_preserve_numeric_schema_and_cut_string_tags() {
    let name = r#"{"name":"read","arguments":{"file_path":"/a/b.ts","limit":55,"offset</arg_key><arg_value>405"}}"#;
    let values = native(&[ev("c1", name, json!({}))]).unwrap();
    assert_eq!(
        args(&values[0]),
        json!({"file_path":"/a/b.ts","limit":55,"offset":405})
    );
    let name = r#"bash_abstract_command_0>{"name":"bash","arguments":{"command":"cd /x && node scan.mjs</description>Re-run scan</arg_value>"#;
    let values = native(&[ev("c1", name, json!({}))]).unwrap();
    assert_eq!(
        args(&values[0]),
        json!({"command":"cd /x && node scan.mjs"})
    );
}
#[test]
fn native_whole_name_supports_input_parameters_flat_fields_and_strings() {
    for name in [
        r#"{"name":"bash","input":{"command":"ls"}}"#,
        r#"{"name":"bash","parameters":{"command":"ls"}}"#,
        r#"{"name":"bash","arguments":"{\"command\":\"ls\"}"}"#,
        r#"{"name":"bash","command":"ls"}"#,
        r#"{ "name": "bash", "arguments": { "command": "ls" } }"#,
    ] {
        let value = native(&[ev("c", name, json!({}))]).unwrap();
        assert_eq!(value[0].name, "bash");
        assert_eq!(args(&value[0]), json!({"command":"ls"}));
    }
}
#[test]
fn native_name_snapshots_grow_without_losing_original_id() {
    let value = native(&[
        ev(
            "n1",
            r#"{"name":"bash","arguments":{"command":"ec"#,
            json!(""),
        ),
        ev(
            "n1",
            r#"{"name":"bash","arguments":{"command":"echo hi"}}"#,
            json!(""),
        ),
    ])
    .unwrap();
    assert_eq!(value[0].id, "n1");
    assert_eq!(value[0].name, "bash");
    assert_eq!(args(&value[0]), json!({"command":"echo hi"}));
}
#[test]
fn resent_snapshot_with_changed_punctuation_is_not_concatenated() {
    let value = native(&[
        ev("n1", "bash", json!(r#"{"command":"echo -"#)),
        ev("n1", "bash", json!(r#"{"command":"echo −"}"#)),
    ])
    .unwrap();
    assert_eq!(args(&value[0]), json!({"command":"echo −"}));
}
#[test]
fn bad_or_unowned_native_fragments_fail_instead_of_becoming_empty_tools() {
    assert!(native(&[ev("n1", "bash", json!(r#"{"command":"cut"#))]).is_err());
    let mut native = NativeCalls::default();
    assert!(native
        .push(&json!({"function":{"arguments":"unowned"}}))
        .is_err());
}
#[test]
fn native_call_can_use_function_call_root_params_and_tool_call_id() {
    let value = native(&[
        json!({"tool_call_id":"real-id","function_call":{"name":"bash"},"params":{"command":"ls"}}),
    ])
    .unwrap();
    assert_eq!(value[0].id, "real-id");
    assert_eq!(args(&value[0]), json!({"command":"ls"}));
}
#[test]
fn wrapper_argument_properties_owned_by_caller_are_never_unwrapped() {
    let call = ToolCall {
        id: "same".into(),
        name: "task".into(),
        arguments: r#"{"input":{"q":1}}"#.into(),
    };
    assert_eq!(
        args(&normalize_call(call, &tools()).unwrap()),
        json!({"input":{"q":1}})
    );
}
#[test]
fn unknown_tools_are_preserved_so_agent_can_answer_explicitly() {
    let value = native(&[ev("original-id", "not_in_request", json!({"x":1}))]).unwrap();
    assert_eq!(value[0].name, "not_in_request");
    assert_eq!(value[0].id, "original-id");
    assert_eq!(args(&value[0]), json!({"x":1}));
}
#[test]
fn excessive_stream_buffers_and_calls_fail_with_sanitized_errors() {
    let mut t = TextTools::new(tools());
    let error = t
        .push(&"private".repeat(MAX_BUFFER / 7 + 1), false)
        .err()
        .unwrap();
    assert!(!error.message.contains("private"));
    let mut calls = NativeCalls::default();
    for i in 0..MAX_CALLS {
        calls
            .push(&ev(&format!("id-{i}"), "bash", json!({})))
            .unwrap();
    }
    assert!(calls.push(&ev("last", "bash", json!({}))).is_err());
}
#[test]
fn completed_stream_cannot_emit_or_replay_another_call() {
    let mut t = TextTools::new(tools());
    t.push("answer", true).unwrap();
    assert!(t.push("more", false).is_err());
}
#[test]
fn native_id_only_announcements_are_bound_to_their_later_name() {
    let value = native(&[
        json!({"id":"announced","index":0,"function":{"arguments":""}}),
        json!({"id":"announced","index":0,"function":{"name":"bash","arguments":{"command":"ls"}}}),
    ])
    .unwrap();
    assert_eq!(value.len(), 1);
    assert_eq!(value[0].id, "announced");
    assert_eq!(args(&value[0]), json!({"command":"ls"}));
}
#[test]
fn numeric_values_that_cannot_be_preserved_are_rejected() {
    assert!(read_with(
        r#"<tool_call>{"name":"task","arguments":{"large":184467440737095516160}}</tool_call>"#,
        0
    )
    .is_err());
}
#[test]
fn named_plain_garbage_does_not_become_an_empty_tool_call() {
    assert!(read_with("<tool_call name=\"bash\">not parameters</tool_call>", 0).is_err());
}
#[test]
fn spaced_reasoning_json_stays_buffered_across_every_header_boundary() {
    let value = read_with("before { \"reasoning_content\" : \"思考\" }after", 1).unwrap();
    assert_eq!(value.reasoning, "思考");
    assert_eq!(value.text, "before after");
}
#[test]
fn normalization_is_idempotent_and_excess_wrappers_fail() {
    let call = ToolCall {
        id: "original".into(),
        name: "write".into(),
        arguments: r#"{"arguments":{"input":{"parameters":{"content":"x"}}}}"#.into(),
    };
    let first = normalize_call(call, &tools()).unwrap();
    let second = normalize_call(first.clone(), &tools()).unwrap();
    assert_eq!(first, second);
    let call = ToolCall {
        id: "original".into(),
        name: "write".into(),
        arguments: r#"{"arguments":{"input":{"parameters":{"args":{"content":"x"}}}}}"#.into(),
    };
    assert!(normalize_call(call, &tools()).is_err());
}
