use super::*;
#[test]
fn composition_wraps_schema_examples_history_and_rebases_local_refs_only() {
    let mut body = json!({"tools":[{"name":"pick","input_schema":{"$schema":"https://json-schema.org/draft/2020-12/schema","anyOf":[{"$ref":"#/$defs/X"},{"$dynamicRef":"#"}],"$defs":{"X":{"type":"string"}},"default":{"$ref":"#leave-data"},"const":{"anyOf":[{"$ref":"#leave-const"}]}},"input_examples":[{"x":1}]}],"messages":[{"role":"assistant","content":[{"type":"tool_use","name":"pick","id":"original-id","input":{"x":18446744073709551615u64}}]}]});
    let names = wrap(&mut body).unwrap();
    assert!(names.contains("pick"));
    let inner = &body["tools"][0]["input_schema"]["properties"]["arguments"];
    assert_eq!(inner["anyOf"][0]["$ref"], "#/properties/arguments/$defs/X");
    assert_eq!(inner["anyOf"][1]["$dynamicRef"], "#/properties/arguments");
    assert_eq!(inner["default"]["$ref"], "#leave-data");
    assert_eq!(inner["const"]["anyOf"][0]["$ref"], "#leave-const");
    assert_eq!(
        body["tools"][0]["input_examples"][0],
        json!({"arguments":{"x":1}})
    );
    assert_eq!(
        body["messages"][0]["content"][0]["input"]["arguments"]["x"],
        18446744073709551615u64
    );
    assert_eq!(body["messages"][0]["content"][0]["id"], "original-id");
}
#[test]
fn schema_resource_ids_named_anchors_and_ordinary_data_keep_scope() {
    let mut value = json!({"anyOf":[{"$id":"https://schema.example/resource","$recursiveRef":"#","$ref":"#/$defs/X"},{"$ref":"#named"}],"$defs":{"X":{"type":"string"}}});
    rebase(&mut value, true, false, 0).unwrap();
    assert_eq!(value["anyOf"][0]["$ref"], "#/$defs/X");
    assert_eq!(value["anyOf"][1]["$ref"], "#named");
}
#[test]
fn unscoped_recursive_schema_refuses_instead_of_changing_meaning() {
    for value in [
        json!({"anyOf":[],"$recursiveAnchor":true}),
        json!({"anyOf":[],"$recursiveRef":"#"}),
    ] {
        let mut body = json!({"tools":[{"name":"f","input_schema":value}]});
        assert!(wrap(&mut body).is_err());
    }
}
#[test]
fn only_exact_wire_wrapper_is_removed() {
    let names = BTreeSet::from(["pick".into()]);
    let mut value = json!({"content":[{"type":"tool_use","id":"id-one","name":"pick","input":{"arguments":{"x":1}}},{"type":"tool_use","id":"id-two","name":"other","input":{"arguments":"user-data"}}]});
    response(&mut value, &names).unwrap();
    assert_eq!(value["content"][0]["input"], json!({"x":1}));
    assert_eq!(
        value["content"][1]["input"],
        json!({"arguments":"user-data"})
    );
    assert!(unwrap(json!({"arguments":{},"extra":1})).is_err());
}
