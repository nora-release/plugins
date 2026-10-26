use super::*;
use crate::tests::{answer, catalog, collect, configured, credentials, Client};
use base64::{engine::general_purpose::STANDARD, Engine};

fn object_union() -> Value {
    json!({"oneOf":[
        {"type":"object","properties":{"mode":{"type":"string","const":"a"}}},
        {"type":"object","properties":{"mode":{"type":"string","const":"b"}}}
    ]})
}

fn parameters(schema: Value) -> Value {
    let chat = json!({
        "messages":[{"role":"user","content":"Do not call tools. Reply exactly OK."}],
        "tools":[{"type":"function","function":{"name":"probe","parameters":schema}}]
    });
    let before = chat.clone();
    let entries = models::listing(&catalog()).unwrap();
    let sent = body(&chat, &entries[0]).unwrap()["tools"][0]["function"]["parameters"].clone();
    assert_eq!(
        chat, before,
        "request construction must leave the caller's tool unchanged"
    );
    sent
}

#[test]
fn object_one_of_is_typed_without_changing_its_branches() {
    let original = object_union();
    let mut expected = original.clone();
    expected["type"] = json!("object");
    assert_eq!(parameters(original), expected);
}

#[test]
fn nested_definitions_and_ref_chains_are_typed_without_inlining_refs() {
    let schema = json!({
        "type":"object",
        "properties":{"s":{"$ref":"#/$defs/union"}},
        "$defs":{
            "union":{"oneOf":[{"$ref":"#/$defs/object"},{"$ref":"#/$defs/alias"}]},
            "object":{"type":"object","properties":{"child":object_union()}},
            "alias":{"$ref":"#/$defs/escaped~1~0"},
            "escaped/~":{"type":"object"}
        }
    });
    let mut expected = schema.clone();
    expected["$defs"]["union"]["type"] = json!("object");
    expected["$defs"]["object"]["properties"]["child"]["type"] = json!("object");
    assert_eq!(parameters(schema), expected);
}

#[test]
fn schema_keywords_recurse_through_single_schemas_lists_and_maps() {
    for keyword in [
        "items",
        "additionalItems",
        "unevaluatedItems",
        "contains",
        "not",
        "if",
        "then",
        "else",
        "propertyNames",
        "additionalProperties",
        "unevaluatedProperties",
    ] {
        let mut schema = json!({"type":"array"});
        schema[keyword] = object_union();
        let mut expected = schema.clone();
        expected[keyword]["type"] = json!("object");
        assert_eq!(
            parameters(schema),
            expected,
            "single-schema keyword {keyword}"
        );
    }
    for keyword in ["anyOf", "oneOf", "allOf", "prefixItems"] {
        let mut schema = json!({"type":"array"});
        schema[keyword] = json!([object_union(), true, {"type":"string"}]);
        let mut expected = schema.clone();
        expected[keyword][0]["type"] = json!("object");
        assert_eq!(
            parameters(schema),
            expected,
            "schema-list keyword {keyword}"
        );
    }
    for keyword in [
        "properties",
        "patternProperties",
        "$defs",
        "definitions",
        "dependentSchemas",
    ] {
        let mut schema = json!({"type":"object"});
        schema[keyword] = json!({"escaped/~":object_union(),"boolean":false});
        let mut expected = schema.clone();
        expected[keyword]["escaped/~"]["type"] = json!("object");
        assert_eq!(parameters(schema), expected, "schema-map keyword {keyword}");
    }
}

#[test]
fn mixed_unions_missing_types_and_invalid_refs_are_not_inferred() {
    let schema = json!({
        "type":"object",
        "properties":{
            "mixed":{"oneOf":[{"type":"object"},{"type":"string"}]},
            "nullable":{"oneOf":[{"type":"object"},{"type":"null"}]},
            "array":{"oneOf":[{"type":"object"},{"type":"array","items":{"type":"string"}}]},
            "any":{"anyOf":[{"type":"object"},{"type":"object"}]},
            "all":{"allOf":[{"type":"object"},{"type":"object"}]},
            "noType":{"properties":{"a":{"type":"string"}}},
            "untypedBranch":{"oneOf":[{"properties":{"a":{"type":"string"}}}]},
            "empty":{"oneOf":[]},
            "notList":{"oneOf":{"type":"object"}},
            "missing":{"oneOf":[{"$ref":"#/$defs/missing"}]},
            "external":{"oneOf":[{"$ref":"other.json#/object"}]},
            "anchor":{"oneOf":[{"$ref":"#object"}]},
            "root":{"oneOf":[{"$ref":"#"}]},
            "loop":{"oneOf":[{"$ref":"#/$defs/A"}]},
            "self":{"oneOf":[{"$ref":"#/$defs/self"}]},
            "notSchema":{"oneOf":[{"$ref":"#/$defs/notSchema"}]},
            "arrayType":{"oneOf":[{"type":["object"]}]},
            "falseBranch":{"oneOf":[{"type":"object"},false]},
            "nullBranch":{"oneOf":[{"type":"object"},null]},
            "refIsNotString":{"oneOf":[{"$ref":true}]}
        },
        "$defs":{
            "A":{"$ref":"#/$defs/B"},
            "B":{"$ref":"#/$defs/A"},
            "self":{"$ref":"#/$defs/self"},
            "notSchema":[{"type":"object"}]
        }
    });
    assert_eq!(parameters(schema.clone()), schema);
}

#[test]
fn existing_types_win_over_both_unions_and_references() {
    for kind in [
        json!("object"),
        json!("string"),
        json!("array"),
        json!(["object", "null"]),
        json!(null),
    ] {
        let mut schema = object_union();
        schema["type"] = kind;
        assert_eq!(parameters(schema.clone()), schema);
    }
    let schema = json!({
        "oneOf":[{"type":"string","$ref":"#/$defs/object"}],
        "$defs":{"object":{"type":"object"}}
    });
    assert_eq!(parameters(schema.clone()), schema);
}

#[test]
fn annotation_values_extensions_and_legacy_tuple_items_are_not_schemas() {
    let mut schema = json!({"type":"object"});
    for keyword in [
        "examples",
        "default",
        "enum",
        "const",
        "x-schema",
        "dependencies",
    ] {
        schema[keyword] = json!({"nested":object_union()});
    }
    schema["items"] = json!([object_union()]);
    assert_eq!(parameters(schema.clone()), schema);
    for value in [
        json!(true),
        json!(false),
        json!(42),
        json!("object"),
        json!([object_union()]),
    ] {
        assert_eq!(parameters(value.clone()), value);
    }
}

#[test]
fn missing_and_null_parameters_use_the_empty_object_schema() {
    let entries = models::listing(&catalog()).unwrap();
    let chat = json!({"messages":[],"tools":[{"type":"function","function":{"name":"probe"}}]});
    assert_eq!(
        body(&chat, &entries[0]).unwrap()["tools"][0]["function"]["parameters"],
        json!({"type":"object","properties":{}})
    );
    assert_eq!(
        parameters(Value::Null),
        json!({"type":"object","properties":{}})
    );
}

#[test]
fn local_ref_chain_limit_matches_the_official_seventeen_hops() {
    for (hops, should_type) in [(1, true), (17, true), (18, false)] {
        let mut definitions = Map::new();
        for index in 0..hops {
            definitions.insert(
                format!("n{index}"),
                if index + 1 == hops {
                    json!({"type":"object"})
                } else {
                    json!({"$ref":format!("#/$defs/n{}",index+1)})
                },
            );
        }
        let schema = json!({"oneOf":[{"$ref":"#/$defs/n0"}],"$defs":definitions});
        let mut expected = schema.clone();
        if should_type {
            expected["type"] = json!("object");
        }
        assert_eq!(parameters(schema), expected, "{hops} local references");
    }
}

#[test]
fn schema_walk_stops_after_sixty_four_levels() {
    for depth in [64, 65] {
        let mut schema = object_union();
        for _ in 0..depth {
            schema = json!({"items":schema});
        }
        let mut expected = schema.clone();
        if depth == 64 {
            let mut leaf = &mut expected;
            for _ in 0..depth {
                leaf = leaf.get_mut("items").unwrap();
            }
            leaf["type"] = json!("object");
        }
        assert_eq!(parameters(schema), expected, "schema depth {depth}");
    }
}

#[test]
fn local_references_see_previously_typed_schema_nodes() {
    let schema = json!({"properties":{
        "a":object_union(),
        "b":{"oneOf":[{"$ref":"#/properties/a"}]}
    }});
    let mut expected = schema.clone();
    expected["properties"]["a"]["type"] = json!("object");
    expected["properties"]["b"]["type"] = json!("object");
    assert_eq!(parameters(schema), expected);
}

#[tokio::test]
async fn all_qoder_sites_send_typed_unions_in_the_encoded_chat_request() {
    for site in [
        Site::International,
        Site::China,
        Site::QwenWork,
        Site::QwenWorkEnterprise,
    ] {
        let host = match site {
            Site::International => "https://api3.qoder.sh",
            Site::China => "https://gateway.qoder.com.cn",
            Site::QwenWork => "https://gateway.qwenwork.cn",
            Site::QwenWorkEnterprise => "https://acme.example.com",
        };
        let mut credentials = credentials();
        if site.is_work() {
            credentials["deviceChat"] = json!(true);
            credentials["access"] = json!("device-one");
        }
        if site == Site::QwenWorkEnterprise {
            credentials["enterpriseHost"] = json!("acme.example.com");
            credentials["vpc"] = json!("acme.example.com");
        }
        let catalog = json!({"app":catalog()["app"],"qwork":catalog()["app"]});
        let client = Client::new(vec![(200, catalog), (200, answer())]);
        let provider = configured(site, client.clone());
        let schema = json!({"type":"object","properties":{"input":object_union()},"examples":[object_union()]});
        let chat = json!({
            "model":"model","messages":[{"role":"user","content":"hello"}],"stream":false,
            "tools":[
                {"type":"function","function":{"name":"probe","description":"Keep description","parameters":schema}},
                {"type":"function","function":{"name":"empty","parameters":null}},
                {"type":"other","function":{"name":"ignored","parameters":schema}}
            ]
        });
        let request: FetchRequest = serde_json::from_value(json!({
            "account":"one","credentials":credentials,"method":"POST",
            "url":format!("{host}/v1/chat/completions"),"body":STANDARD.encode(chat.to_string())
        }))
        .unwrap();
        let response = provider.fetch(request).await.unwrap();
        assert_eq!(response.status, 200);
        assert!(String::from_utf8(collect(response).await)
            .unwrap()
            .contains("hello"));
        let requests = client.requests.lock().unwrap();
        assert_eq!(requests.len(), 2);
        assert_eq!(requests[1].url, format!("{host}{CHAT_PATH}"));
        let plain = codec::decode(std::str::from_utf8(requests[1].body.as_ref().unwrap()).unwrap())
            .unwrap();
        let sent: Value = serde_json::from_str(&plain).unwrap();
        let mut expected = chat["tools"].as_array().unwrap()[..2].to_vec();
        expected[0]["function"]["parameters"]["properties"]["input"]["type"] = json!("object");
        expected[1]["function"]["parameters"] = json!({"type":"object","properties":{}});
        expected[1]["function"]["description"] = json!("");
        assert_eq!(
            sent["tools"],
            json!(expected),
            "encoded request for {site:?}"
        );
    }
}
