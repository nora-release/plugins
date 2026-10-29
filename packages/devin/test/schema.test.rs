use super::*;
#[test]
fn root_unions_merge_fields_and_require_only_common_branch_keys() {
    let input = json!({"oneOf":[{"type":"object","properties":{"mode":{"const":"a"},"x":{"type":"string"}},"required":["mode","x"]},{"type":"object","properties":{"mode":{"const":"b"},"y":{"type":"number"}},"required":["mode","y"]}],"description":"preserve"});
    let output = object_root(&input).unwrap();
    assert_eq!(output["type"], "object");
    assert_eq!(output["required"], json!(["mode"]));
    assert!(output["properties"].get("x").is_some());
    assert!(output["properties"].get("y").is_some());
    assert_eq!(
        output["properties"]["mode"]["anyOf"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    assert_eq!(output["description"], "preserve");
    assert!(input.get("oneOf").is_some());
}
#[test]
fn local_refs_nested_compositions_and_allof_required_fields_are_kept() {
    let input = json!({"$defs":{"A":{"type":"object","properties":{"a":{"type":"string"}},"required":["a"]}},"allOf":[{"$ref":"#/$defs/A"}],"anyOf":[{"oneOf":[{"type":"object","properties":{"b":{"type":"integer"}},"required":["b"]},{"type":"object","properties":{"c":{"type":"boolean"}}}]}]});
    let output = object_root(&input).unwrap();
    assert_eq!(output["required"], json!(["a"]));
    assert!(output["properties"].get("a").is_some());
    assert!(output["properties"].get("b").is_some());
    assert!(output["properties"].get("c").is_some());
    assert!(output.get("allOf").is_none());
}
#[test]
fn gemini_nullable_arrays_and_enum_inference_are_recursive_without_data_changes() {
    let input = json!({"properties":{"s":{"type":["string","null"]},"a":{"type":"array"},"e":{"enum":[1,2,null]},"data":{"const":{"type":["keep","null"]}}}});
    let output = gemini(&input, 0).unwrap();
    assert_eq!(output["type"], "object");
    assert_eq!(output["properties"]["s"]["type"], "string");
    assert_eq!(output["properties"]["s"]["nullable"], true);
    assert_eq!(output["properties"]["a"]["items"], json!({}));
    assert_eq!(output["properties"]["e"]["type"], "integer");
    assert_eq!(output["properties"]["e"]["enum"], json!([1, 2]));
    assert_eq!(
        output["properties"]["data"]["const"],
        input["properties"]["data"]["const"]
    );
}
#[test]
fn ordinary_object_schema_passes_through_unchanged() {
    let schema =
        json!({"type":"object","properties":{"x":{"type":"string"}},"additionalProperties":false});
    assert_eq!(object_root(&schema).unwrap(), schema);
}
