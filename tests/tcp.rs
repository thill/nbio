use tcp_stream::TLSConfig;

use nbio::{
    tcp::{TcpServer, TcpSession},
    ConnectionStatus, ReadStatus, Session, WriteStatus,
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

    // construct read buffer and a large payload to write
    let mut read_buffer = Vec::new();
    let mut write_payload = Vec::new();
    for i in 0..9999999 {
        write_payload.push(i as u8)
    }

    // send the message with the client while reading it with the server session
    let mut remaining = write_payload.as_slice();
    while let WriteStatus::Pending(pw) = client.write(remaining).unwrap() {
        remaining = pw;
        if let ReadStatus::Data(read) = session.read().unwrap() {
            read_buffer.extend_from_slice(read);
        }
    }

    // read the rest of the message with the server session
    while read_buffer.len() < 9999999 {
        if let ReadStatus::Data(read) = session.read().unwrap() {
            read_buffer.extend_from_slice(read);
        }
    }

    // validate the received message
    assert_eq!(read_buffer.len(), write_payload.len());
    assert_eq!(read_buffer, write_payload);
}

#[test]
pub fn tcp_tls() {
    // tls client
    let mut client = TcpSession::connect("www.google.com:443")
        .unwrap()
        .into_tls("www.google.com", TLSConfig::default())
        .unwrap();

    while client.status() != ConnectionStatus::Connected {
        client.drive().unwrap();
    }

    // send request
    let request = "GET / HTTP/1.1\r\nhost: www.google.com\r\n\r\n"
        .as_bytes()
        .to_vec();
    let mut remaining = request.as_slice();
    while let Ok(WriteStatus::Pending(pw)) = client.write(remaining) {
        remaining = pw;
        client.drive().unwrap();
    }

    // read (some of) response
    let mut response = Vec::new();
    while response.len() < 9 {
        if let ReadStatus::Data(read) = client.read().unwrap() {
            response.extend_from_slice(read);
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

    // send 100,000 messages with client while "slowly" reading with session
    let mut received: Vec<u8> = Vec::new();
    let mut backpressure = false;
    for i in 0..100000 {
        let write_payload = format!("test test test test hello world {i:06}!");
        // send the message with the client while reading it with the server session
        let mut remaining = write_payload.as_bytes();
        while let WriteStatus::Pending(pw) = client.write(remaining).unwrap() {
            remaining = pw;
            backpressure = true;
            // only read when backpressure is encountered to simulate a slow consumer
            for _ in 0..10 {
                if let ReadStatus::Data(read) = session.read().unwrap() {
                    received.extend_from_slice(&read);
                }
            }
        }
    }

    // assert backpressure and write failures were tested
    assert!(backpressure);

    // finish reading with session until all 100,000 messages of length=39 were received while driving client to write completion
    while received.len() < (100000 * 39) {
        client.drive().unwrap();
        if let ReadStatus::Data(read) = session.read().unwrap() {
            received.extend_from_slice(&read);
        }
    }
    assert_eq!(received.len(), 100000 * 39)
}
