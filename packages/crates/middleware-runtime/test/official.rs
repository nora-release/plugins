use nora_gateway_middleware::{Action, Context, Middleware};
use serde_json::{json, Value};
pub fn run<M: Middleware>(cases: &str) {
    let cases: Value = serde_json::from_str(cases).unwrap();
    for c in cases.as_array().unwrap() {
        let name = c["name"].as_str().unwrap();
        let mut ctx = Context {
            protocol: c["ctx"]["protocol"].as_str().unwrap_or("anthropic").into(),
            model: c["ctx"]["model"].as_str().unwrap_or("").into(),
            path: c["ctx"]["path"].as_str().unwrap_or("").into(),
            agent: c["ctx"]["agent"].as_str().unwrap_or("").into(),
            stream: c["ctx"]["stream"].as_bool().unwrap_or(false),
            options: c.get("options").cloned().unwrap_or(json!({})),
            state: json!({}),
            status: c["status"].as_u64().unwrap_or(200) as u16,
            ..Default::default()
        };
        let m = M::default();
        if let Some(original) = c.get("request") {
            let mut b = original.clone();
            let a = m
                .apply("request", &mut b, &mut ctx)
                .unwrap_or_else(|e| panic!("{name}: {e}"));
            if let Some(reject) = c.get("reject") {
                let Action::Reject { status, message } = a else {
                    panic!("{name}: expected reject, got {a:?}")
                };
                assert_eq!(status as u64, reject["status"].as_u64().unwrap(), "{name}");
                if let Some(expected) = reject["message"].as_str() {
                    assert!(message.contains(expected), "{name}: {message}");
                }
            } else {
                assert!(!matches!(a, Action::Reject { .. }), "{name}: {a:?}");
                if let Some(p) = c.get("provider") {
                    ctx.provider = p.clone();
                    m.apply("provider", &mut b, &mut ctx).unwrap();
                }
                assert_eq!(b, *c.get("expect_request").unwrap_or(original), "{name}");
            }
        }
        if let Some(events) = c["events"].as_array() {
            let mut actual = vec![];
            for e in events {
                let mut b = e["data"].clone();
                if b.is_object() || b.is_array() {
                    if m.apply("event", &mut b, &mut ctx).unwrap() == Action::Drop {
                        continue;
                    }
                }
                actual.push(b);
            }
            let expected = c.get("expect_events").cloned().unwrap_or_else(|| {
                json!(events.iter().map(|e| e["data"].clone()).collect::<Vec<_>>())
            });
            assert_eq!(json!(actual), expected, "{name}");
        }
        if let Some(original) = c.get("response") {
            let mut b = original.clone();
            m.apply("response", &mut b, &mut ctx).unwrap();
            assert_eq!(b, *c.get("expect_response").unwrap_or(original), "{name}");
        }
    }
}
