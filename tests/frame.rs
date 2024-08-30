#[cfg(any(feature = "tcp"))]
mod tests {
    use nbio::{
        frame::{FrameDuplex, U64FrameDeserializer, U64FrameSerializer},
        tcp::{TcpServer, TcpSession},
        Publish, PublishOutcome, Receive, ReceiveOutcome, Session, SessionStatus,
    };

    #[test]
    fn one_small_frame() {
        // create server, connect client, establish server session
        let server = TcpServer::bind("127.0.0.1:34001").unwrap();
        let client = TcpSession::connect("127.0.0.1:34001").unwrap();
        let session = server.accept().unwrap().unwrap().0;

        let mut client = FrameDuplex::new(
            client,
            U64FrameDeserializer::new(),
            U64FrameSerializer::new(),
            1024,
        );
        let mut session = FrameDuplex::new(
            session,
            U64FrameDeserializer::new(),
            U64FrameSerializer::new(),
            1024,
        );

        while client.status() == SessionStatus::Establishing
            || session.status() == SessionStatus::Establishing
        {
            client.drive().unwrap();
            session.drive().unwrap();
        }

        // construct receive holder and a large payload to publish
        let mut receive_payload = None;
        let mut publish_payload = Vec::new();
        for i in 0..512 {
            publish_payload.push(i as u8)
        }

        // send the message with the client while receiveing it with the server session
        let mut remaining = publish_payload.as_slice();
        while let PublishOutcome::Incomplete(pw) = client.publish(remaining).unwrap() {
            remaining = pw;
            client.drive().unwrap();
            if let ReceiveOutcome::Payload(receive) = session.receive().unwrap() {
                receive_payload = Some(Vec::from(receive));
            }
        }

        // drive publish from client while receiveing single payload from session
        while let None = receive_payload {
            client.drive().unwrap();
            if let ReceiveOutcome::Payload(receive) = session.receive().unwrap() {
                receive_payload = Some(Vec::from(receive));
            }
        }
        let receive_payload = receive_payload.unwrap();

        // validate the received message
        assert_eq!(receive_payload.len(), publish_payload.len());
        assert_eq!(receive_payload, publish_payload);
    }

    #[test]
    fn one_large_frame() {
        // create server, connect client, establish server session
        let server = TcpServer::bind("127.0.0.1:34002").unwrap();
        let client = TcpSession::connect("127.0.0.1:34002").unwrap();
        let session = server.accept().unwrap().unwrap().0;

        let mut client = FrameDuplex::new(
            client,
            U64FrameDeserializer::new(),
            U64FrameSerializer::new(),
            1024,
        );
        let mut session = FrameDuplex::new(
            session,
            U64FrameDeserializer::new(),
            U64FrameSerializer::new(),
            1024,
        );

        while client.status() == SessionStatus::Establishing
            || session.status() == SessionStatus::Establishing
        {
            client.drive().unwrap();
            session.drive().unwrap();
        }

        // construct receive holder and payload larger than the publish buffer
        let mut receive_payload = None;
        let mut publish_payload = Vec::new();
        for i in 0..888888 {
            publish_payload.push(i as u8)
        }

        // send the message with the client while receiveing it with the server session
        let mut remaining = publish_payload.as_slice();
        while let PublishOutcome::Incomplete(pw) = client.publish(remaining).unwrap() {
            remaining = pw;
            if let ReceiveOutcome::Payload(receive) = session.receive().unwrap() {
                receive_payload = Some(Vec::from(receive));
            }
        }

        // drive publish from client while receiveing single payload from session
        while let None = receive_payload {
            client.drive().unwrap();
            if let ReceiveOutcome::Payload(receive) = session.receive().unwrap() {
                receive_payload = Some(Vec::from(receive));
            }
        }
        let receive_payload = receive_payload.unwrap();

        // validate the received message
        assert_eq!(receive_payload.len(), publish_payload.len());
        assert_eq!(receive_payload, publish_payload);
    }

    #[test]
    fn framing_slow_consumer() {
        // create server, connect client, establish server session
        let server = TcpServer::bind("127.0.0.1:34003").unwrap();
        let client = TcpSession::connect("127.0.0.1:34003").unwrap();
        let session = server.accept().unwrap().unwrap().0;

        // use a small publish buffer to stress test
        let mut client = FrameDuplex::new(
            client,
            U64FrameDeserializer::new(),
            U64FrameSerializer::new(),
            1024,
        );
        let mut session = FrameDuplex::new(
            session,
            U64FrameDeserializer::new(),
            U64FrameSerializer::new(),
            1024,
        );

        while client.status() == SessionStatus::Establishing
            || session.status() == SessionStatus::Establishing
        {
            client.drive().unwrap();
            session.drive().unwrap();
        }

        // send 100,000 messages with client while "slowly" receiveing with session
        let mut received = Vec::new();
        let mut backpressure = false;
        for i in 0..100000 {
            let m = format!("test test test test hello world {i:06}!");
            // send the message with the client while receiveing it with the server session
            let mut remaining = m.as_bytes();
            while let PublishOutcome::Incomplete(pw) = client.publish(remaining).unwrap() {
                client.drive().unwrap();
                remaining = pw;
                backpressure = true;
                // only receive when backpressure is encountered to simulate a slow consumer
                for _ in 0..10 {
                    if let ReceiveOutcome::Payload(receive) = session.receive().unwrap() {
                        received.push(String::from_utf8_lossy(receive).to_string());
                    }
                }
            }
            client.drive().unwrap();
        }

        // assert backpressure and publish failures were tested
        assert!(backpressure);

        // finish receiveing with session until all 100,000 messages were received while driving client to publish completion
        while received.len() < 100000 {
            client.drive().unwrap();
            if let ReceiveOutcome::Payload(receive) = session.receive().unwrap() {
                received.push(String::from_utf8_lossy(receive).to_string());
            }
        }

        // validate the received messages
        for i in 0..100000 {
            assert_eq!(
                received.get(i).expect(&format!("message idx {i}")),
                &format!("test test test test hello world {i:06}!")
            );
        }
    }
}
