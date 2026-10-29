use nora_gateway_middleware::{Context, Middleware};
use nora_provider_think_tags::Plugin;
use serde_json::{json, Value};
const EXAMPLE: &str =
    "<think>开始分析。使用 `<think>...</think>` 标签。\n继续分析。</think>\n最终正文。";
fn stream(parts: &[String], protocol: &str) -> String {
    let mut context = Context {
        protocol: protocol.into(),
        stream: true,
        options: json!({}),
        state: json!({}),
        ..Default::default()
    };
    let mut output = String::new();
    for part in parts {
        let mut event = if protocol == "chat" {
            json!({"choices":[{"index":0,"delta":{"content":part}}]})
        } else {
            json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":part}})
        };
        Plugin.apply("event", &mut event, &mut context).unwrap();
        output.push_str(
            if protocol == "chat" {
                event.pointer("/choices/0/delta/content")
            } else {
                event.pointer("/delta/text")
            }
            .and_then(Value::as_str)
            .unwrap_or_default(),
        );
    }
    if protocol == "chat" {
        let mut finish = json!({"choices":[{"index":0,"delta":{},"finish_reason":"stop"}]});
        Plugin.apply("event", &mut finish, &mut context).unwrap();
        output.push_str(
            finish
                .pointer("/choices/0/delta/content")
                .and_then(Value::as_str)
                .unwrap_or_default(),
        );
    }
    output
}
#[test]
fn close_tag_examples_are_code_in_streamed_and_whole_replies() {
    for protocol in ["chat", "anthropic"] {
        assert_eq!(stream(&[EXAMPLE.into()], protocol), "最终正文。");
    }
    for (mut body, pointer) in [
        (
            json!({"choices":[{"message":{"content":EXAMPLE}}]}),
            "/choices/0/message/content",
        ),
        (
            json!({"content":[{"type":"text","text":EXAMPLE}]}),
            "/content/0/text",
        ),
        (
            json!({"output":[{"type":"message","content":[{"type":"output_text","text":EXAMPLE}]}]}),
            "/output/0/content/0/text",
        ),
    ] {
        Plugin
            .apply(
                "response",
                &mut body,
                &mut Context {
                    protocol: "chat".into(),
                    options: json!({}),
                    state: json!({}),
                    status: 200,
                    ..Default::default()
                },
            )
            .unwrap();
        assert_eq!(body.pointer(pointer).unwrap(), "最终正文。");
    }
}
#[test]
fn every_unicode_character_is_an_independent_delta() {
    let parts = EXAMPLE.chars().map(|c| c.to_string()).collect::<Vec<_>>();
    for protocol in ["chat", "anthropic"] {
        assert_eq!(stream(&parts, protocol), "最终正文。");
    }
}
#[test]
fn every_utf8_byte_is_an_independent_latin1_delta() {
    let parts = EXAMPLE
        .bytes()
        .map(|b| char::from(b).to_string())
        .collect::<Vec<_>>();
    for protocol in ["chat", "anthropic"] {
        let result = stream(&parts, protocol);
        assert_eq!(
            String::from_utf8(result.chars().map(|c| c as u8).collect()).unwrap(),
            "最终正文。"
        );
    }
}
macro_rules! case {
    ($name:ident,$parts:expr,$expected:expr) => {
        #[test]
        fn $name() {
            let parts: Vec<String> = $parts;
            for protocol in ["chat", "anthropic"] {
                assert_eq!(stream(&parts, protocol), $expected);
            }
        }
    };
}
case!(
    double_backtick_span,
    vec!["<think>a ``x`</think>`` b</think>c".into()],
    "c"
);
case!(
    fenced_block,
    "<think>like\n```xml\n</think>\n```\nso</think>\nok"
        .chars()
        .map(|c| c.to_string())
        .collect(),
    "ok"
);
case!(
    span_ends_with_line,
    vec!["<think>a `b\n</think>\nok".into()],
    "ok"
);
case!(
    backticks_after_close,
    "<think>a</think>\n`x` y"
        .chars()
        .map(|c| c.to_string())
        .collect(),
    "`x` y"
);
case!(
    split_close,
    vec!["<think>a `".into(), "x` b</th".into(), "ink>c".into()],
    "c"
);
case!(
    trailing_backticks_remain_thinking,
    vec!["<think>a ``".into()],
    ""
);
#[test]
fn whole_unclosed_code_is_not_silently_deleted() {
    for (text, expected) in [
        ("<think>use `</think>` here", "<think>use `</think>` here"),
        ("<think>like\n```\n</think>\n```\n</think>\n\nok", "ok"),
    ] {
        let mut body = json!({"choices":[{"message":{"content":text}}]});
        Plugin
            .apply(
                "response",
                &mut body,
                &mut Context {
                    protocol: "chat".into(),
                    options: json!({}),
                    state: json!({}),
                    status: 200,
                    ..Default::default()
                },
            )
            .unwrap();
        assert_eq!(body["choices"][0]["message"]["content"], expected);
    }
}
