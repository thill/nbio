use std::{
    sync::atomic::{AtomicUsize, Ordering},
    time::{Duration, SystemTime},
};

use nbio::{
    mock::MockSession,
    util::{HeartbeatResult, HeartbeatingSession, LivenessSession, LivenessStrategy},
    ReadStatus, Session,
};

#[test]
fn test_heartbeat() {
    let seq = AtomicUsize::new(0);
    let mut sess = HeartbeatingSession::new(
        MockSession::new(),
        Duration::from_millis(50),
        move |s: &mut dyn Session<WriteData = usize, ReadData = usize>| {
            s.write(&seq.fetch_add(1, Ordering::AcqRel))?;
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

    // assert at least 3 and at most 5 heartbeats received
    assert_eq!(0, sess.session_mut().write_queue.pop_front().unwrap());
    assert_eq!(1, sess.session_mut().write_queue.pop_front().unwrap());
    assert_eq!(2, sess.session_mut().write_queue.pop_front().unwrap());
    assert!(sess.session().write_queue.len() <= 2);
}

#[test]
fn test_liveness() {
    let mut sess = LivenessSession::new(
        MockSession::new(),
        Duration::from_millis(50),
        LivenessStrategy::default(),
    );

    // send and drive for longer than interval
    let end = SystemTime::now() + Duration::from_millis(100);
    while SystemTime::now() < end {
        sess.session_mut()
            .read_result_queue
            .push_back(Ok(ReadStatus::Data(&(0 as usize))));
        sess.read().unwrap();
        sess.write(&(0 as usize)).unwrap();
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
