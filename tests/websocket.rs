#[cfg(any(feature = "websocket"))]
mod tests {
    use std::time::{Duration, SystemTime};

    use nbio::{
        websocket::{Message, WebSocketSession},
        Publish, Receive, ReceiveOutcome, Session, SessionStatus,
    };

    #[test]
    fn websocket_echo() {
        let timeout = SystemTime::now() + Duration::from_secs(5);

        // connect
        let mut session =
            WebSocketSession::connect("wss://echo.websocket.org/", None, None).unwrap();
        while session.status() == SessionStatus::Establishing && SystemTime::now() < timeout {
            session.drive().unwrap();
        }
        assert_eq!(session.status(), SessionStatus::Established);

        // send messages
        session
            .publish(Message::Text("hello, world!".into()))
            .unwrap();
        session
            .publish(Message::Text("hello again, world!".into()))
            .unwrap();
        session
            .publish(Message::Text("it's peanut butter jelly time!".into()))
            .unwrap();
        session
            .publish(Message::Text("peanut butter jelly!".into()))
            .unwrap();
        session
            .publish(Message::Binary(vec![0, 1, 2, 3].into()))
            .unwrap();

        // wait until received all messages
        let mut received = Vec::new();
        while received.len() < 5 && SystemTime::now() < timeout {
            session.drive().unwrap();
            if let ReceiveOutcome::Payload(m) = session.receive().unwrap() {
                println!("RECEIVED: {m:?}");
                if let Message::Text(x) = &m {
                    if x.starts_with("Request served by ") {
                        // skip greeting message
                        continue;
                    }
                }
                received.push(m.into_owned());
            }
        }

        // validate messages
        assert_eq!(unwrap_text(received.get(0)), "hello, world!");
        assert_eq!(unwrap_text(received.get(1)), "hello again, world!");
        assert_eq!(
            unwrap_text(received.get(2)),
            "it's peanut butter jelly time!"
        );
        assert_eq!(unwrap_text(received.get(3)), "peanut butter jelly!");
        assert_eq!(unwrap_binary(received.get(4)), &vec![0, 1, 2, 3]);
    }

    fn unwrap_text<'a>(message: Option<&'a Message>) -> &'a str {
        match message.unwrap() {
            Message::Text(text) => text.as_ref(),
            _ => panic!("not Text"),
        }
    }

    fn unwrap_binary<'a>(message: Option<&'a Message>) -> &'a [u8] {
        match message.unwrap() {
            Message::Binary(bin) => bin.as_ref(),
            _ => panic!("not Binary"),
        }
    }
}
