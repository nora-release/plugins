use serde_json::{json, Value};

#[derive(Default, Debug, PartialEq)]
pub struct Observed {
    pub text: String,
    pub calls: Vec<Value>,
    pub error: Option<String>,
}

pub const ARRAY_CALL: &str = "<function=compress><parameter=content>[{\"summary\":\"TASK（历史）\",\"endId\":\"m03680\"}]</parameter></function>";
pub const CALLS: &str = "<function_calls><invoke name=\"image_result\"> <parameter name=\"id\">image-24</parameter> </invoke> <invoke name=\"image_result\"> <parameter name=\"id\">image-25</parameter> </invoke> <invoke name=\"image_result\"> <parameter name=\"id\">image-26</parameter> </invoke> </function_calls>";
pub const STRING_CALL: &str = "<function=bash><parameter=command>[1, 2]</parameter></function>";
pub const CUT: &str = "<function=bash><parameter=command>ls -";

pub fn tools() -> Vec<Value> {
    json!([
        {"type":"function","function":{"name":"compress","parameters":{"type":"object","properties":{"content":{"type":"array","items":{"type":"object"}}}}}},
        {"type":"function","function":{"name":"image_result","parameters":{"type":"object","properties":{"id":{"type":"string"}}}}},
        {"type":"function","function":{"name":"bash","parameters":{"type":"object","properties":{"command":{"type":"string"}}}}}
    ]).as_array().unwrap().clone()
}

pub fn pieces(text: &str, size: usize) -> Vec<String> {
    let chars: Vec<_> = text.chars().collect();
    chars
        .chunks(if size == 0 { chars.len().max(1) } else { size })
        .map(|s| s.iter().collect())
        .collect()
}

pub fn expected_calls() -> Vec<Value> {
    ["24", "25", "26"]
        .into_iter()
        .map(|n| json!(["image_result", {"id":format!("image-{n}")}]))
        .collect()
}

pub fn plain_text() -> Vec<String> {
    vec![
        "Claude wraps its calls in <function_calls> tags; each is an <invoke> block.".into(),
        format!("Use `{CALLS}` to call it."),
        format!("Example:\n```xml\n{CALLS}\n```\nThat is the format."),
        "Qwen writes `<function=bash>` and then its parameters.".into(),
        "The <invoke name=\"bash\"> tag names a tool, and the call ends later.".into(),
        "a < b and c > d, <function_calls without a close".into(),
        "<function_calls><invoke name=\"delete_everything\"><parameter name=\"path\">/</parameter></invoke></function_calls>".into(),
        "<function=nope><parameter=x>1</parameter></function>".into(),
        "<function=nope><parameter=x>unclosed".into(),
    ]
}
