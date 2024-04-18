use std::{
    sync::atomic::{AtomicUsize, Ordering},
    time::{Duration, SystemTime},
};

use nbio::{
    mock::MockSession,
    util::{HeartbeatResult, HeartbeatingSession, LivenessSession, LivenessStrategy},
    Session,
};

#[test]
fn test_heartbeat() {
    let seq = AtomicUsize::new(0);
    let mut sess = HeartbeatingSession::new(
        MockSession::<[usize], [usize]>::new(),
        Duration::from_millis(50),
        move |s| {
            s.write(&[seq.fetch_add(1, Ordering::AcqRel)])?;
            Ok(HeartbeatResult::Sent)
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
            .write_queue
            .pop_front()
            .unwrap()
            .get(0)
            .unwrap()
    );
    assert_eq!(
        1,
        *sess
            .session_mut()
            .write_queue
            .pop_front()
            .unwrap()
            .get(0)
            .unwrap()
    );
    assert!(sess.session().write_queue.len() <= 3);
}

#[test]
fn test_liveness() {
    let mut sess = LivenessSession::new(
        MockSession::<[u8], [u8]>::new(),
        Duration::from_millis(50),
        LivenessStrategy::default(),
    );

    // send and drive for longer than interval
    let end = SystemTime::now() + Duration::from_millis(100);
    while SystemTime::now() < end {
        sess.session_mut().read_queue.push_back(vec![0]);
        sess.read().unwrap();
        sess.write(&[0]).unwrap();
        assert!(sess.session_mut().write_queue.pop_front().is_some());
        sess.drive().unwrap();
        std::thread::sleep(Duration::from_millis(1));
    }

    // assert drive does not report liveness error
    while sess.drive().unwrap() {}
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
