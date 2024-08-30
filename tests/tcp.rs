#[cfg(any(feature = "tcp"))]
mod tests {
    use tcp_stream::TLSConfig;

    use nbio::{
        tcp::{TcpServer, TcpSession},
        Publish, PublishOutcome, Receive, ReceiveOutcome, Session, SessionStatus,
    };

    #[test]
    pub fn tcp_client_server() {
        // create server, connect client, establish server session
        let server = TcpServer::bind("127.0.0.1:33001").unwrap();
        let mut client = TcpSession::connect("127.0.0.1:33001").unwrap();
        let mut session = None;
        while let None = session {
            session = server.accept().unwrap().map(|(s, _)| s);
        }
        let mut session = session.unwrap();

        while client.status() == SessionStatus::Establishing
            || session.status() == SessionStatus::Establishing
        {
            client.drive().unwrap();
            session.drive().unwrap();
        }

        // construct receive buffer and a large payload to publish
        let mut receive_buffer = Vec::new();
        let mut publish_payload = Vec::new();
        for i in 0..9999999 {
            publish_payload.push(i as u8)
        }

        // send the message with the client while receiveing it with the server session
        let mut remaining = publish_payload.as_slice();
        while let PublishOutcome::Incomplete(pw) = client.publish(remaining).unwrap() {
            remaining = pw;
            if let ReceiveOutcome::Payload(receive) = session.receive().unwrap() {
                receive_buffer.extend_from_slice(receive);
            }
        }

        // receive the rest of the message with the server session
        while receive_buffer.len() < 9999999 {
            if let ReceiveOutcome::Payload(receive) = session.receive().unwrap() {
                receive_buffer.extend_from_slice(receive);
            }
        }

        // validate the received message
        assert_eq!(receive_buffer.len(), publish_payload.len());
        assert_eq!(receive_buffer, publish_payload);
    }

    #[test]
    pub fn tcp_tls_after_establishing() {
        // tls client
        let mut client = TcpSession::connect("www.google.com:443").unwrap();

        while client.status() == SessionStatus::Establishing {
            client.drive().unwrap();
        }
        assert_eq!(client.status(), SessionStatus::Established);

        let mut client = client
            .into_tls("www.google.com", TLSConfig::default())
            .unwrap();
        while client.status() == SessionStatus::Establishing {
            client.drive().unwrap();
        }
        assert_eq!(client.status(), SessionStatus::Established);

        // send request
        let request = "GET / HTTP/1.1\r\nhost: www.google.com\r\n\r\n"
            .as_bytes()
            .to_vec();
        let mut remaining = request.as_slice();
        while let Ok(PublishOutcome::Incomplete(pw)) = client.publish(remaining) {
            remaining = pw;
            client.drive().unwrap();
        }

        // receive (some of) response
        let mut response = Vec::new();
        while response.len() < 9 {
            if let ReceiveOutcome::Payload(receive) = client.receive().unwrap() {
                response.extend_from_slice(receive);
            }
        }

        assert!(String::from_utf8_lossy(&response).starts_with("HTTP/1.1 "));
    }

    #[test]
    pub fn tcp_tls_before_establishing() {
        // tls client
        let mut client = TcpSession::connect("www.google.com:443")
            .unwrap()
            .into_tls("www.google.com", TLSConfig::default())
            .unwrap();

        while client.status() == SessionStatus::Establishing {
            client.drive().unwrap();
        }
        assert_eq!(client.status(), SessionStatus::Established);

        // send request
        let request = "GET / HTTP/1.1\r\nhost: www.google.com\r\n\r\n"
            .as_bytes()
            .to_vec();
        let mut remaining = request.as_slice();
        while let Ok(PublishOutcome::Incomplete(pw)) = client.publish(remaining) {
            remaining = pw;
            client.drive().unwrap();
        }

        // receive (some of) response
        let mut response = Vec::new();
        while response.len() < 9 {
            if let ReceiveOutcome::Payload(receive) = client.receive().unwrap() {
                response.extend_from_slice(receive);
            }
        }

        assert!(String::from_utf8_lossy(&response).starts_with("HTTP/1.1 "));
    }

    #[test]
    pub fn tcp_slow_consumer() {
        // create server, connect client, establish server session
        let server = TcpServer::bind("127.0.0.1:33002").unwrap();
        let mut client = TcpSession::connect("127.0.0.1:33002").unwrap();
        let mut session = server.accept().unwrap().unwrap().0;

        while client.status() == SessionStatus::Establishing
            || session.status() == SessionStatus::Establishing
        {
            client.drive().unwrap();
            session.drive().unwrap();
        }

        // send 100,000 messages with client while "slowly" receiveing with session
        let mut received: Vec<u8> = Vec::new();
        let mut backpressure = false;
        for i in 0..100000 {
            let publish_payload = format!("test test test test hello world {i:06}!");
            // send the message with the client while receiveing it with the server session
            let mut remaining = publish_payload.as_bytes();
            while let PublishOutcome::Incomplete(pw) = client.publish(remaining).unwrap() {
                remaining = pw;
                backpressure = true;
                // only receive when backpressure is encountered to simulate a slow consumer
                for _ in 0..10 {
                    if let ReceiveOutcome::Payload(receive) = session.receive().unwrap() {
                        received.extend_from_slice(&receive);
                    }
                }
            }
        }

        // assert backpressure and publish failures were tested
        assert!(backpressure);

        // finish receiveing with session until all 100,000 messages of length=39 were received while driving client to publish completion
        while received.len() < (100000 * 39) {
            client.drive().unwrap();
            if let ReceiveOutcome::Payload(receive) = session.receive().unwrap() {
                received.extend_from_slice(&receive);
            }
        }
        assert_eq!(received.len(), 100000 * 39)
    }
}
