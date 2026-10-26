use super::*;
fn connection() -> (Pipe, tokio::net::UnixStream, tokio::net::UnixStream) {
    let (writer, commands) = tokio::net::UnixStream::pair().unwrap();
    let (responses, reader) = tokio::net::UnixStream::pair().unwrap();
    (Pipe::new(writer, reader), commands, responses)
}
async fn command(socket: &mut tokio::net::UnixStream) -> Value {
    let mut bytes = Vec::new();
    loop {
        let byte = socket.read_u8().await.unwrap();
        if byte == 0 {
            break;
        }
        bytes.push(byte);
    }
    serde_json::from_slice(&bytes).unwrap()
}
#[tokio::test]
async fn private_pipe_matches_out_of_order_responses_and_session_events() {
    let (pipe, mut commands, mut responses) = connection();
    let mut events = pipe.subscribe();
    let server = tokio::spawn(async move {
        let one = command(&mut commands).await;
        let two = command(&mut commands).await;
        assert_eq!(one["sessionId"], "private-session");
        let replies = [
            json!({"sessionId":"private-session","method":"Page.loaded","params":{"ready":true}}),
            json!({"id":two["id"],"result":{"value":2}}),
            json!({"id":one["id"],"result":{"value":1}}),
        ];
        for reply in replies {
            let bytes = serde_json::to_vec(&reply).unwrap();
            let middle = bytes.len() / 2;
            responses.write_all(&bytes[..middle]).await.unwrap();
            tokio::task::yield_now().await;
            responses.write_all(&bytes[middle..]).await.unwrap();
            responses.write_u8(0).await.unwrap();
        }
    });
    let (one, two) = tokio::join!(
        pipe.send("Page.one", json!({}), Some("private-session")),
        pipe.send("Page.two", json!({}), None)
    );
    assert_eq!(one.unwrap()["value"], 1);
    assert_eq!(two.unwrap()["value"], 2);
    assert_eq!(events.recv().await.unwrap()["params"]["ready"], true);
    server.await.unwrap();
}
#[tokio::test]
async fn connection_close_settles_pending_commands_and_refusals_are_bounded() {
    let (pipe, mut commands, mut responses) = connection();
    let server = tokio::spawn(async move {
        let one = command(&mut commands).await;
        responses
            .write_all(
                format!(
                    "{}\0",
                    json!({"id":one["id"],"error":{"message":"x".repeat(1000)}})
                )
                .as_bytes(),
            )
            .await
            .unwrap();
        let _ = command(&mut commands).await;
        drop(responses);
    });
    assert_eq!(
        pipe.send("bad", json!({}), None)
            .await
            .unwrap_err()
            .message
            .len(),
        512
    );
    assert!(pipe.send("pending", json!({}), None).await.is_err());
    assert!(pipe.closed());
    server.await.unwrap();
}
#[tokio::test]
async fn canceling_a_command_removes_its_waiter_without_closing_the_pipe() {
    let (pipe, mut commands, _responses) = connection();
    let task = {
        let pipe = pipe.clone();
        tokio::spawn(async move { pipe.send("wait", json!({}), None).await })
    };
    let _ = command(&mut commands).await;
    task.abort();
    let _ = task.await;
    assert!(pipe.0.pending.lock().unwrap().is_empty());
    assert!(!pipe.closed());
    pipe.stop();
}
#[test]
fn only_navigation_context_failures_are_retryable() {
    assert!(context_changed("Execution context was destroyed"));
    assert!(context_changed("Cannot find context with specified id"));
    assert!(!context_changed("A JavaScript expression is invalid"));
    assert!(!truthy(&Value::Null));
    assert!(!truthy(&json!(false)));
    assert!(truthy(&json!([])));
}
