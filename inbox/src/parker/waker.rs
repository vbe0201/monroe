use std::{
    cell::UnsafeCell,
    sync::atomic::{AtomicU8, Ordering},
    task::{Poll, Waker},
};

#[inline]
fn will_wake(current: &Option<Waker>, other: &Waker) -> bool {
    current
        .as_ref()
        .map(|w| w.will_wake(other))
        .unwrap_or(false)
}

pub struct AtomicWaker {
    state: AtomicU8,
    waker: UnsafeCell<Option<Waker>>,
}

impl AtomicWaker {
    const IDLE: u8 = 0;
    const UPDATING: u8 = 1;
    const WAITING: u8 = 2;
    const NOTIFIED: u8 = 3;

    pub const fn new() -> Self {
        Self {
            state: AtomicU8::new(Self::IDLE),
            waker: UnsafeCell::new(None),
        }
    }

    pub fn poll_ready(&self, waker: &Waker) -> Poll<()> {
        let state = self.state.load(Ordering::Acquire);
        if state == Self::NOTIFIED {
            return Poll::Ready(());
        }

        debug_assert!(state == Self::IDLE || state == Self::WAITING);
        if let Err(state) =
            self.state
                .compare_exchange(state, Self::UPDATING, Ordering::Acquire, Ordering::Acquire)
        {
            debug_assert_eq!(state, Self::NOTIFIED);
            return Poll::Ready(());
        }

        unsafe {
            let w = &mut *self.waker.get();
            if !will_wake(w, waker) {
                *w = Some(waker.clone());
            }
        }

        match self.state.compare_exchange(
            Self::UPDATING,
            Self::WAITING,
            Ordering::AcqRel,
            Ordering::Acquire,
        ) {
            Ok(_) => Poll::Pending,
            Err(Self::NOTIFIED) => Poll::Ready(()),
            Err(_) => unreachable!(),
        }
    }

    pub fn wake(&self) {
        if self.state.swap(Self::NOTIFIED, Ordering::AcqRel) == Self::WAITING {
            if let Some(waker) = unsafe { (*self.waker.get()).take() } {
                waker.wake();
            }
        }
    }
}
