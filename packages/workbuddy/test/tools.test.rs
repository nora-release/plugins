use super::*;

#[test]
fn tool_names_are_collision_safe_and_responses_map_back_to_the_original_names() {
    let mut body = json!({"tools":[{"type":"function","function":{"name":"read_file","parameters":{"type":"object"}}},{"type":"function","function":{"name":"read.file","parameters":{"enum":["a",1,null]}}}],"messages":[{"role":"assistant","tool_calls":[{"function":{"name":"read.file","arguments":"{}"}}]}],"tool_choice":{"type":"function","function":{"name":"read.file"}}});
    let names = fit(&mut body).unwrap();
    assert_eq!(body["tools"][1]["function"]["name"], "read_file_2");
    assert_eq!(
        body["messages"][0]["tool_calls"][0]["function"]["name"],
        "read_file_2"
    );
    assert_eq!(body["tool_choice"]["function"]["name"], "read_file_2");
    let mut response = json!({"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"name":"read_file_2"}}]}}]});
    rename(&mut response, &names);
    assert_eq!(
        response["choices"][0]["delta"]["tool_calls"][0]["function"]["name"],
        "read.file"
    );
}

#[test]
fn schemas_fix_boolean_subschemas_tuple_items_and_mixed_enum_types() {
    let mut schema = json!({"type":"OBJECT","required":"value","properties":{"optional":true,"denied":false,"choice":{"enum":["a",1,null]},"tuple":{"items":[{"type":"string"},{"type":"number"}]},"pattern":{"type":"string","pattern":r"[^\0]"}}});
    fix_schema(&mut schema, 0).unwrap();
    assert_eq!(schema["type"], "object");
    assert!(schema.get("required").is_none());
    assert_eq!(schema["properties"]["optional"], json!({}));
    assert_eq!(schema["properties"]["denied"], json!({"not":{}}));
    assert!(schema["properties"]["choice"]["anyOf"].is_array());
    assert!(schema["properties"]["tuple"]["items"]["anyOf"].is_array());
    assert_eq!(schema["properties"]["pattern"]["pattern"], r"[^\u0000]");
}
