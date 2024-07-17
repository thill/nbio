// docker run --rm --ipc=host  -u $(id -u ${USER}):$(id -g ${USER}) neomantra/aeron-cpp-debian:latest

#[cfg(any(feature = "aeron"))]
mod tests {
    use std::{
        sync::{Arc, Mutex},
        thread,
        time::{Duration, SystemTime},
    };

    use nbio::{
        aeron::{
            AeronMarker, BorrowedMessage, Buffer, Client, Context, Message, OwnedMessage,
            Publication, PublishMessage, Subscription,
        },
        Callback, Publish, Receive, ReceiveOutcome, Session, SessionStatus,
    };

    fn setup<P: AeronMarker, S: AeronMarker>(
        stream_id: i32,
        callback: Option<Box<dyn for<'a> Callback<BorrowedMessage<'a>> + 'static>>,
    ) -> (Publication<P>, Subscription<S>) {
        let context = Context::new()
            .unwrap()
            .with_dir("/dev/shm/aeron-default")
            .unwrap();

        let client = Arc::new(Client::new(context).unwrap());
        client.start().unwrap();

        let mut publication =
            Publication::new(Arc::clone(&client), "aeron:ipc".to_owned(), stream_id);
        while publication.status() == SessionStatus::Establishing {
            publication.drive().unwrap();
            thread::yield_now();
        }
        if publication.status() != SessionStatus::Established {
            panic!("publication could not be established");
        }

        let mut subscription =
            Subscription::new(Arc::clone(&client), "aeron:ipc".to_owned(), stream_id);
        if let Some(callback) = callback {
            subscription = subscription.with_callback(callback).unwrap();
        }

        while subscription.status() == SessionStatus::Establishing {
            subscription.drive().unwrap();
            thread::yield_now();
        }
        if subscription.status() != SessionStatus::Established {
            panic!("subscription could not be established");
        }

        (publication, subscription)
    }

    #[test]
    fn test_publish_receive() {
        let (mut p, mut s) = setup::<Buffer, Buffer>(1, None);

        let timeout = SystemTime::now() + Duration::from_secs(5);

        assert!(p.publish("hello world!".as_bytes()).unwrap().is_published());

        let mut received = Vec::<Vec<u8>>::new();
        while SystemTime::now() < timeout && received.len() < 1 {
            s.drive().unwrap();
            if let ReceiveOutcome::Payload(x) = s.receive().unwrap() {
                received.push(x);
            }
        }
        assert_eq!(1, received.len());
        assert_eq!("hello world!".as_bytes(), received.get(0).unwrap());

        assert!(p.publish("number 2!".as_bytes()).unwrap().is_published());
        assert!(p.publish("number 3!".as_bytes()).unwrap().is_published());

        let mut s = s;

        let mut received = Vec::new();
        while SystemTime::now() < timeout && received.len() < 2 {
            s.drive().unwrap();
            if let ReceiveOutcome::Payload(x) = s.receive().unwrap() {
                received.push(x);
            }
        }

        assert_eq!(2, received.len());
        assert_eq!("number 2!".as_bytes(), received.get(0).unwrap());
        assert_eq!("number 3!".as_bytes(), received.get(1).unwrap());
    }

    #[test]
    fn test_publish_callback() {
        let received = Arc::new(Mutex::new(Vec::new()));
        let (mut p, mut s) = {
            let received = Arc::clone(&received);
            setup::<Buffer, Buffer>(
                2,
                Some(Box::new(move |x: BorrowedMessage| {
                    received.lock().unwrap().push(x.payload.to_vec())
                })),
            )
        };

        let timeout = SystemTime::now() + Duration::from_secs(5);

        assert!(p.publish("hello world!".as_bytes()).unwrap().is_published());
        assert!(p.publish("number 2!".as_bytes()).unwrap().is_published());
        assert!(p.publish("number 3!".as_bytes()).unwrap().is_published());

        while SystemTime::now() < timeout && received.lock().unwrap().len() < 3 {
            s.drive().unwrap();
        }
        assert_eq!(3, received.lock().unwrap().len());
        assert_eq!(
            "hello world!".as_bytes(),
            received.lock().unwrap().get(0).unwrap()
        );
        assert_eq!(
            "number 2!".as_bytes(),
            received.lock().unwrap().get(1).unwrap()
        );
        assert_eq!(
            "number 3!".as_bytes(),
            received.lock().unwrap().get(2).unwrap()
        );
    }

    #[test]
    fn test_reserved_value() {
        let (mut p, mut s) = setup::<Message, Message>(3, None);

        let timeout = SystemTime::now() + Duration::from_secs(5);

        let pub_msg = PublishMessage {
            buffer: "hello world!".as_bytes(),
            reserved_value: 42,
        };
        assert!(p.publish(pub_msg).unwrap().is_published());

        let mut received = Vec::<OwnedMessage>::new();
        while SystemTime::now() < timeout && received.len() < 1 {
            s.drive().unwrap();
            if let ReceiveOutcome::Payload(x) = s.receive().unwrap() {
                received.push(x);
            }
        }
        assert_eq!(1, received.len());
        let rcv_msg = received.get(0).unwrap();
        assert_eq!(
            "hello world!",
            String::from_utf8(rcv_msg.payload.clone()).unwrap()
        );
        assert_eq!(42, rcv_msg.reserved_value);
    }
}
