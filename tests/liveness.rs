use std::{
    sync::atomic::{AtomicUsize, Ordering},
    time::{Duration, SystemTime},
};

use nbio::{
    liveness::{HeartbeatOutcome, HeartbeatingSession, LivenessSession, LivenessStrategy},
    mock::MockSession,
    DriveOutcome, Publish, Receive, Session,
};

#[test]
fn test_heartbeat() {
    let seq = AtomicUsize::new(0);
    let mut sess = HeartbeatingSession::new(
        MockSession::<&[usize], Vec<usize>>::new(),
        Duration::from_millis(50),
        move |s| {
            s.publish(vec![seq.fetch_add(1, Ordering::AcqRel)])?;
            Ok(HeartbeatOutcome::Sent)
        },
    );

    // drive for more than 3 intervals
    let end = SystemTime::now() + Duration::from_millis(151);
    while SystemTime::now() < end {
        sess.drive().unwrap();
        std::thread::sleep(Duration::from_millis(1));
    }
    sess.drive().unwrap();

    // assert at least 2 and at most 5 heartbeats received
    assert_eq!(
        0,
        *sess
            .session_mut()
            .publish_queue
            .pop_front()
            .unwrap()
            .get(0)
            .unwrap()
    );
    assert_eq!(
        1,
        *sess
            .session_mut()
            .publish_queue
            .pop_front()
            .unwrap()
            .get(0)
            .unwrap()
    );
    assert!(sess.session().publish_queue.len() <= 3);
}

#[test]
fn test_liveness() {
    let mut sess = LivenessSession::new(
        MockSession::<&[u8], &[u8]>::new(),
        Duration::from_millis(50),
        LivenessStrategy::default(),
    );

    // send and drive for longer than interval
    let end = SystemTime::now() + Duration::from_millis(100);
    while SystemTime::now() < end {
        sess.session_mut().receive_queue.push_back(&[0]);
        sess.receive().unwrap();
        sess.publish(&[0]).unwrap();
        assert!(sess.session_mut().publish_queue.pop_front().is_some());
        sess.drive().unwrap();
        std::thread::sleep(Duration::from_millis(1));
    }

    // assert drive does not report liveness error
    while sess.drive().unwrap() == DriveOutcome::Active {}
    assert!(sess.drive().is_ok());

    // drive for longer than interval
    let end = SystemTime::now() + Duration::from_millis(100);
    while SystemTime::now() < end {
        sess.drive().ok();
        std::thread::sleep(Duration::from_millis(1));
    }

    // assert drive reports liveness error
    assert!(sess.drive().is_err());
}
