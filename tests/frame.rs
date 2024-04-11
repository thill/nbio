use nbio::{
    frame::{FramingSession, U64FramingStrategy},
    tcp::{StreamingTcpSession, TcpServer},
    ReadStatus, Session, WriteStatus,
};

#[test]
fn one_small_frame() {
    // create server, connect client, establish server session
    let server = TcpServer::bind("127.0.0.1:34001").unwrap();
    let client = StreamingTcpSession::connect("127.0.0.1:34001")
        .unwrap()
        .with_nonblocking(true)
        .unwrap();
    let session = server
        .accept()
        .unwrap()
        .unwrap()
        .0
        .with_nonblocking(true)
        .unwrap();

    let mut client = FramingSession::new(client, U64FramingStrategy::new(), 1024);
    let mut session = FramingSession::new(session, U64FramingStrategy::new(), 1024);

    // construct read holder and a large payload to write
    let mut read_payload = None;
    let mut write_payload = Vec::new();
    for i in 0..512 {
        write_payload.push(i as u8)
    }

    // send the message with the client while reading it with the server session
    let mut remaining = write_payload.as_slice();
    while let WriteStatus::Pending(pw) = client.write(remaining).unwrap() {
        remaining = pw;
        client.drive().unwrap();
        if let ReadStatus::Data(read) = session.read().unwrap() {
            read_payload = Some(Vec::from(read));
        }
    }

    // drive write from client while reading single payload from session
    while let None = read_payload {
        client.drive().unwrap();
        if let ReadStatus::Data(read) = session.read().unwrap() {
            read_payload = Some(Vec::from(read));
        }
    }
    let read_payload = read_payload.unwrap();

    // validate the received message
    assert_eq!(read_payload.len(), write_payload.len());
    assert_eq!(read_payload, write_payload);
}

#[test]
fn one_large_frame() {
    // create server, connect client, establish server session
    let server = TcpServer::bind("127.0.0.1:34002").unwrap();
    let client = StreamingTcpSession::connect("127.0.0.1:34002")
        .unwrap()
        .with_nonblocking(true)
        .unwrap();
    let session = server
        .accept()
        .unwrap()
        .unwrap()
        .0
        .with_nonblocking(true)
        .unwrap();

    let mut client = FramingSession::new(client, U64FramingStrategy::new(), 1024);
    let mut session = FramingSession::new(session, U64FramingStrategy::new(), 1024);

    // construct read holder and payload larger than the write buffer
    let mut read_payload = None;
    let mut write_payload = Vec::new();
    for i in 0..888888 {
        write_payload.push(i as u8)
    }

    // send the message with the client while reading it with the server session
    let mut remaining = write_payload.as_slice();
    while let WriteStatus::Pending(pw) = client.write(remaining).unwrap() {
        remaining = pw;
        if let ReadStatus::Data(read) = session.read().unwrap() {
            read_payload = Some(Vec::from(read));
        }
    }

    // drive write from client while reading single payload from session
    while let None = read_payload {
        client.drive().unwrap();
        if let ReadStatus::Data(read) = session.read().unwrap() {
            read_payload = Some(Vec::from(read));
        }
    }
    let read_payload = read_payload.unwrap();

    // validate the received message
    assert_eq!(read_payload.len(), write_payload.len());
    assert_eq!(read_payload, write_payload);
}

#[test]
fn framing_slow_consumer() {
    // create server, connect client, establish server session
    let server = TcpServer::bind("127.0.0.1:34003").unwrap();
    let client = StreamingTcpSession::connect("127.0.0.1:34003")
        .unwrap()
        .with_nonblocking(true)
        .unwrap();
    let session = server
        .accept()
        .unwrap()
        .unwrap()
        .0
        .with_nonblocking(true)
        .unwrap();

    // use a small write buffer to stress test
    let mut client = FramingSession::new(client, U64FramingStrategy::new(), 1024);
    let mut session = FramingSession::new(session, U64FramingStrategy::new(), 1024);

    // send 100,000 messages with client while "slowly" reading with session
    let mut received = Vec::new();
    let mut backpressure = false;
    for i in 0..100000 {
        let m = format!("test test test test hello world {i:06}!");
        // send the message with the client while reading it with the server session
        let mut remaining = m.as_bytes();
        while let WriteStatus::Pending(pw) = client.write(remaining).unwrap() {
            client.drive().unwrap();
            remaining = pw;
            backpressure = true;
            // only read when backpressure is encountered to simulate a slow consumer
            for _ in 0..10 {
                if let ReadStatus::Data(read) = session.read().unwrap() {
                    received.push(String::from_utf8_lossy(read).to_string());
                }
            }
        }
        client.drive().unwrap();
    }

    // assert backpressure and write failures were tested
    assert!(backpressure);

    // finish reading with session until all 100,000 messages were received while driving client to write completion
    while received.len() < 100000 {
        client.drive().unwrap();
        if let ReadStatus::Data(read) = session.read().unwrap() {
            received.push(String::from_utf8_lossy(read).to_string());
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
