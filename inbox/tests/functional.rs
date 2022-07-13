use std::sync::atomic::{AtomicBool, Ordering};

use monroe_inbox::*;
use static_assertions::{assert_impl_all, assert_not_impl_any};

#[allow(unused)]
struct NotSendSync(*mut ());

#[test]
fn sender_is_send_sync() {
    assert_impl_all!(Sender<()>: Send, Sync);
    assert_not_impl_any!(Sender<NotSendSync>: Send, Sync);
}

#[test]
fn receiver_is_send_sync() {
    assert_impl_all!(Receiver<()>: Send, Sync);
    assert_not_impl_any!(Receiver<NotSendSync>: Send, Sync);
}

#[test]
fn sender_is_clone() {
    assert_impl_all!(Sender<NotSendSync>: Clone);
    assert_not_impl_any!(Sender<NotSendSync>: Copy);
}

#[test]
fn receiver_is_not_clone() {
    assert_not_impl_any!(Receiver<NotSendSync>: Clone, Copy);
}

#[test]
#[should_panic = "capacity of 0 is not allowed!"]
fn capacity_of_zero() {
    let _ = channel::<()>(0);
}

#[test]
#[should_panic = "capacity must not exceed isize::MAX"]
fn capacity_too_large() {
    let _ = channel::<()>(isize::MAX as _);
}

#[test]
fn matching_ids() {
    let (tx, rx) = channel::<()>(1);
    assert_eq!(tx.id(), rx.id());
    assert_eq!(tx.clone().id(), rx.id());

    let (tx2, rx2) = channel::<()>(1);
    assert_eq!(tx2.id(), rx2.id());
    assert_ne!(tx.id(), tx2.id());
}

#[test]
fn sender_is_connected() {
    let (tx, rx) = channel::<()>(1);
    assert!(tx.is_connected());
    drop(rx);
    assert!(!tx.is_connected());
}

#[test]
fn receiver_is_connected() {
    let (tx, rx) = channel::<()>(1);
    assert!(rx.is_connected());
    drop(tx);
    assert!(!rx.is_connected());
}

#[test]
fn sending_and_receiving_value() {
    let (tx, mut rx) = channel::<u32>(1);
    tx.try_send(1337).unwrap();
    assert_eq!(rx.try_recv(), Ok(1337));
}

#[test]
fn sending_and_receiving_interleaved() {
    let (tx, mut rx) = channel::<u32>(5);
    for i in 0..10 {
        tx.try_send(i).unwrap();
        assert_eq!(rx.try_recv(), Ok(i));
    }
}

#[test]
fn send_receive_errors() {
    let (tx, mut rx) = channel::<u32>(1);

    assert_eq!(rx.try_recv(), Err(TryRecvError::Empty));
    tx.try_send(1).unwrap();
    assert_eq!(tx.try_send(2), Err(TrySendError::Full(2)));

    assert_eq!(rx.try_recv(), Ok(1));
    assert_eq!(rx.try_recv(), Err(TryRecvError::Empty));
    drop(tx);
    assert_eq!(rx.try_recv(), Err(TryRecvError::Disconnected));

    let tx = rx.make_sender();
    drop(rx);
    assert_eq!(tx.try_send(3), Err(TrySendError::Disconnected(3)));
}

#[test]
fn channel_drop_buffered_values() {
    static DROPPED: AtomicBool = AtomicBool::new(false);

    #[derive(Debug)]
    struct DropPanic;

    impl Drop for DropPanic {
        fn drop(&mut self) {
            DROPPED.store(true, Ordering::Relaxed);
        }
    }

    let channels = channel::<DropPanic>(1);
    channels.0.try_send(DropPanic).unwrap();
    drop(channels);

    assert_eq!(DROPPED.load(Ordering::Relaxed), true);
}
