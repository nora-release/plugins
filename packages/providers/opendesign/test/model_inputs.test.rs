use super::*;

#[tokio::test]
async fn seed_keeps_known_audio_and_video_input_without_promoting_generic_files_to_pdf() {
    let (plugin, client) = setup(vec![]);
    let listing = plugin.models(CallContext::default()).await.unwrap();
    let models = listing["models"].as_array().unwrap();
    for id in ["mimo-v2.6-flash", "mimo-v2.6-pro"] {
        let model = models.iter().find(|model| model["id"] == id).unwrap();
        assert_eq!(model["input"], json!(["text", "image", "audio", "video"]));
    }
    let glm = models
        .iter()
        .find(|model| model["id"] == "glm-5.3-flash")
        .unwrap();
    assert_eq!(glm["input"], json!(["text", "image", "video"]));
    let gpt = models
        .iter()
        .find(|model| model["id"] == "gpt-6-luna")
        .unwrap();
    assert_eq!(gpt["input"], json!(["text", "image"]));
    assert!(client.requests.lock().unwrap().is_empty());
}

#[tokio::test]
async fn live_input_types_survive_the_provider_response_as_unique_supported_kinds() {
    let (plugin, client) = setup(vec![(
        200,
        json!({"data":[
            {"id":"all-inputs","architecture":{"input_modalities":["pdf","video","text","audio","image","audio","file","future",null,12]}},
            {"id":"audio-only","architecture":{"input_modalities":["audio"]}},
            {"id":"file-only","architecture":{"input_modalities":["file"]}}
        ]}),
    )]);
    let listing = plugin.models(context()).await.unwrap();
    assert_eq!(listing["source"], "remote");
    assert_eq!(
        listing["models"][0]["input"],
        json!(["text", "image", "audio", "video", "pdf"])
    );
    assert_eq!(listing["models"][1]["input"], json!(["text", "audio"]));
    assert_eq!(listing["models"][2]["input"], json!(["text"]));
    for model in listing["models"].as_array().unwrap() {
        let decoded: nora_ai::Model = serde_json::from_value(model.clone()).unwrap();
        assert_eq!(
            serde_json::to_value(decoded).unwrap()["input"],
            model["input"]
        );
        assert_ne!(model["info"]["imageOutput"], true);
        assert_ne!(model["info"]["videoOutput"], true);
    }
    assert_eq!(client.requests.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn a_live_catalog_never_inherits_input_capabilities_missing_from_its_entry() {
    for architecture in [
        Value::Null,
        json!({}),
        json!({"input_modalities":null}),
        json!({"input_modalities":"image"}),
        json!({"input_modalities":[]}),
        json!({"input_modalities":["text"]}),
    ] {
        let (plugin, _) = setup(vec![(
            200,
            json!({"data":[{"id":"mimo-v2.6-flash","architecture":architecture}]}),
        )]);
        let listing = plugin.models(context()).await.unwrap();
        assert_eq!(
            listing["models"][0]["input"],
            json!(["text"]),
            "{architecture}"
        );
        assert_eq!(listing["models"][0]["name"], "MiMo V2.6 Flash");
    }
}
