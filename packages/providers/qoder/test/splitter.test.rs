use super::*;
#[test]
fn split_text_xml_and_json_calls_are_lifted_only_when_complete() {
    let mut splitter = Splitter::default();
    let mut pieces = splitter.feed("before <tool_");
    pieces.extend(splitter.feed("call><function=read><parameter=path>file</parameter><parameter=limit>3</parameter></function></tool_call> after"));
    pieces.extend(splitter.flush());
    assert!(matches!(&pieces[0],Piece::Text(value) if value=="before "));
    let call = pieces
        .iter()
        .find_map(|piece| {
            if let Piece::Call { name, args } = piece {
                Some((name, args))
            } else {
                None
            }
        })
        .unwrap();
    assert_eq!(call.0, "read");
    assert_eq!(
        serde_json::from_str::<Value>(call.1).unwrap(),
        json!({"path":"file","limit":3})
    );
    assert!(splitter.saw_tool);
    let mut json = Splitter::default();
    json.feed("<tool_call>{\"name\":\"read\",\"arguments\":{}}");
    assert!(matches!(json.flush().as_slice(),[Piece::Call{name,..}] if name=="read"));
}
#[test]
fn incomplete_or_unreadable_calls_fail_without_becoming_user_visible_answers() {
    for input in [
        "<tool_call>{\"name\":\"read\",\"arguments\":",
        "<tool_call><function=read>broken</tool_call>",
    ] {
        let mut splitter = Splitter::default();
        let mut pieces = splitter.feed(input);
        pieces.extend(splitter.flush());
        assert!(
            pieces.iter().any(|piece| matches!(piece, Piece::Error(_))),
            "{input}"
        );
    }
    let mut quoted = Splitter::default();
    let mut pieces = quoted.feed("```\n<tool_call>{incomplete");
    pieces.extend(quoted.flush());
    assert!(!pieces.iter().any(|piece| matches!(piece, Piece::Error(_))));
}
