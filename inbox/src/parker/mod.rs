use std::{
    future::Future,
    marker::PhantomPinned,
    mem::drop as unlock,
    pin::Pin,
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
    task::{Context, Poll, RawWaker, RawWakerVTable, Waker},
    thread,
};

use usync::{const_mutex, Mutex};

mod wait_queue;
use self::wait_queue::{WaitQueue, Waiter};

mod waker;

struct Signal {
    thread: thread::Thread,
    notified: AtomicBool,
    _pin: PhantomPinned,
}

impl Signal {
    fn current() -> Self {
        Self {
            thread: thread::current(),
            notified: AtomicBool::new(false),
            _pin: PhantomPinned,
        }
    }
}

const VTABLE: RawWakerVTable = RawWakerVTable::new(
    |ptr| RawWaker::new(ptr, &VTABLE),
    |ptr| unsafe {
        let signal = &*ptr.cast::<Signal>();
        if !signal.notified.swap(true, Ordering::Release) {
            signal.thread.unpark();
        }
    },
    |_| unreachable!("wake_by_ref"),
    |_| {},
);

pub struct Parker {
    pending: AtomicUsize,
    waiters: Mutex<WaitQueue>,
}

impl Parker {
    pub const fn new() -> Self {
        Self {
            pending: AtomicUsize::new(0),
            waiters: const_mutex(WaitQueue::new()),
        }
    }

    unsafe fn block_on_pinned<F: Future>(mut fut: Pin<&mut F>) -> F::Output {
        let signal = Signal::current();
        let signal = Pin::new_unchecked(&signal);

        let ptr = (&*signal as *const Signal).cast::<()>();
        let waker = Waker::from_raw(RawWaker::new(ptr, &VTABLE));
        let mut cx = Context::from_waker(&waker);

        loop {
            if let Poll::Ready(output) = fut.as_mut().poll(&mut cx) {
                return output;
            }

            while !signal.notified.swap(false, Ordering::Acquire) {
                thread::park();
            }
        }
    }

    #[allow(clippy::await_holding_lock)] // XXX: False positive report.
    pub async fn park_one(&self, should_park: impl FnOnce() -> bool) {
        struct Park<'a, 'b> {
            parker: &'a Parker,
            waiter: Option<Pin<&'b Waiter>>,
        }

        impl<'a, 'b> Future for Park<'a, 'b> {
            type Output = ();

            fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
                let waiter = self.waiter.take().expect("Park polled after completion");
                if waiter.poll_ready(cx.waker()).is_ready() {
                    return Poll::Ready(());
                }

                self.waiter = Some(waiter);
                Poll::Pending
            }
        }

        impl<'a, 'b> Drop for Park<'a, 'b> {
            fn drop(&mut self) {
                if let Some(waiter) = self.waiter.take() {
                    unsafe {
                        if self.parker.waiters.lock().try_remove(waiter.as_ref()) {
                            self.parker.pending.fetch_sub(1, Ordering::Relaxed);
                            return;
                        }

                        self.waiter = Some(waiter);
                        Parker::block_on_pinned(Pin::new(self));
                    }
                }
            }
        }

        self.pending.fetch_add(1, Ordering::SeqCst);
        let mut waiters = self.waiters.lock();

        if !should_park() {
            unlock(waiters);
            self.pending.fetch_sub(1, Ordering::Relaxed);
            return;
        }

        unsafe {
            let waiter = Waiter::new();
            let waiter = Pin::new_unchecked(&waiter);

            waiters.push(waiter.as_ref());
            unlock(waiters);

            Park {
                parker: self,
                waiter: Some(waiter),
            }
            .await
        }
    }

    pub fn unpark_one(&self) {
        if self.pending.load(Ordering::SeqCst) == 0 {
            return;
        }

        unsafe {
            let mut waiters = self.waiters.lock();
            if let Some(waiter) = waiters.pop() {
                unlock(waiters);
                self.pending.fetch_sub(1, Ordering::Relaxed);
                waiter.as_ref().wake();
            }
        }
    }

    pub fn unpark_all(&self) {
        if self.pending.load(Ordering::SeqCst) == 0 {
            return;
        }

        unsafe {
            let drain = self.waiters.lock().drain();

            if !drain.is_empty() {
                self.pending.fetch_sub(drain.len(), Ordering::Relaxed);
            }

            for waiter in drain {
                waiter.as_ref().wake();
            }
        }
    }
}
