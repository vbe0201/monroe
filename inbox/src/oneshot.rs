//! *Oneshot* channels which enable passing a value from a
//! [`Sender`] to a [`Receiver`] exactly once.
//!
//! Each channel pair is intended for single-time use and
//! the [`Sender`] will be consumed after its first-time use.
//!
//! Since [`Sender::send`] is not async, it can be used from
//! anywhere in code.

use std::{
    cell::UnsafeCell,
    fmt,
    future::Future,
    mem::MaybeUninit,
    pin::Pin,
    ptr::NonNull,
    sync::atomic::{AtomicU8, Ordering},
    task::{Poll, Waker},
};

use usync::{const_mutex, Mutex};

use crate::{RecvError, SendError, TryRecvError};

mod state {
    pub const RECEIVER_ALIVE: u8 = 1 << (u8::BITS - 1);
    pub const SENDER_ALIVE: u8 = 1 << (u8::BITS - 2);

    pub const SLOT_FILLED: u8 = 1;

    #[inline(always)]
    pub const fn initial() -> u8 {
        RECEIVER_ALIVE | SENDER_ALIVE
    }

    #[inline(always)]
    pub const fn receiver_alive(state: u8) -> bool {
        state & RECEIVER_ALIVE != 0
    }

    #[inline(always)]
    pub const fn sender_alive(state: u8) -> bool {
        state & SENDER_ALIVE != 0
    }

    #[inline(always)]
    pub const fn slot_filled(state: u8) -> bool {
        state & SLOT_FILLED != 0
    }
}

struct Channel<T> {
    state: AtomicU8,
    value: UnsafeCell<MaybeUninit<T>>,
    receiver: Mutex<Option<Waker>>,
}

impl<T> Channel<T> {
    fn alloc() -> NonNull<Self> {
        let alloc = Box::new(Self {
            state: AtomicU8::new(state::initial()),
            value: UnsafeCell::new(MaybeUninit::uninit()),
            receiver: const_mutex(None),
        });

        // SAFETY: `Box` allocations are non-null.
        unsafe { NonNull::new_unchecked(Box::into_raw(alloc)) }
    }
}

impl<T> Drop for Channel<T> {
    fn drop(&mut self) {
        let old = self.state.fetch_and(!state::SLOT_FILLED, Ordering::Relaxed);
        if state::slot_filled(old) {
            unsafe {
                self.value.get_mut().assume_init_drop();
            }
        }
    }
}

/// Creates a new oneshot channel and returns both halves of it
/// for communication between asynchronous tasks.
///
/// This channel supports sharing **exactly one** value from
/// the [`Sender`] to its [`Receiver`].
///
/// This channel type is therefore only suitable for single-time
/// use.
pub fn channel<T>() -> (Sender<T>, Receiver<T>) {
    let channel = Channel::alloc();
    (Sender { channel }, Receiver { channel })
}

/// The sending half of a channel which can publish up to
/// one `T` value to the [`Receiver`].
///
/// New instances are created using the [`channel`] function.
///
/// This is for single-time use only.
pub struct Sender<T> {
    channel: NonNull<Channel<T>>,
}

impl<T> Sender<T> {
    #[inline]
    fn channel(&self) -> &Channel<T> {
        // SAFETY: `self.channel` is a valid `Box` allocation
        // and the constrained `&self` lifetime cannot make it
        // outlive its dropping point.
        unsafe { self.channel.as_ref() }
    }

    /// Indicates whether the [`Receiver`] is still connected.
    pub fn is_connected(&self) -> bool {
        let state = self.channel().state.load(Ordering::Relaxed);
        state::receiver_alive(state)
    }

    /// Indicates whether the given [`Receiver`] receives its
    /// messages from this [`Sender`].
    #[inline]
    pub fn sends_to(&self, receiver: &Receiver<T>) -> bool {
        self.channel == receiver.channel
    }

    /// Consumes this sender and publishes `value` to the
    /// channel.
    ///
    /// On error, ownership of that value is transferred back
    /// in [`SendError`].
    pub fn send(self, value: T) -> Result<(), SendError<T>> {
        // Check if the receiving half is still alive,
        // no point in sending otherwise.
        if !self.is_connected() {
            return Err(SendError(value));
        }

        let channel = self.channel();

        // Write the value to the slot.
        // SAFETY: We're the only sender.
        unsafe {
            channel.value.get().write(MaybeUninit::new(value));
        }

        // Mark the slot as filled.
        let old = channel
            .state
            .fetch_add(state::SLOT_FILLED, Ordering::AcqRel);
        debug_assert!(!state::slot_filled(old));

        Ok(())
    }
}

impl<T> fmt::Debug for Sender<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Sender").finish()
    }
}

impl<T> Drop for Sender<T> {
    fn drop(&mut self) {
        let channel = self.channel();

        // Mark the Sender as dropped.
        let old = channel
            .state
            .fetch_and(!state::SENDER_ALIVE, Ordering::AcqRel);

        // If the Receiver is still around, we need to wake it.
        if state::receiver_alive(old) {
            if let Some(waker) = channel.receiver.lock().take() {
                waker.wake();
            }
            return;
        }

        // As the last channel instance, we have to free the memory.
        // SAFETY: `Channel`s are originally allocated as `Box`es.
        unsafe { drop(Box::from_raw(self.channel.as_ptr())) }
    }
}

// SAFETY: `Sender`s are safe to share across thread boundaries
// as long as the values they're sending also are. Sufficient
// synchronization for all operations is done internally.
unsafe impl<T: Send> Send for Sender<T> {}
unsafe impl<T: Send> Sync for Sender<T> {}

impl<T> Unpin for Sender<T> {}

/// The receiving half of a channel which receives up to a
/// single `T` value published by a [`Sender`].
///
/// New instances are created using the [`channel`] function.
///
/// This is for single-time use only.
pub struct Receiver<T> {
    channel: NonNull<Channel<T>>,
}

impl<T> Receiver<T> {
    #[inline]
    fn channel(&self) -> &Channel<T> {
        // SAFETY: `self.channel` is a valid `Box` allocation
        // and the constrained `&self` lifetime cannot make it
        // outlive its dropping point.
        unsafe { self.channel.as_ref() }
    }

    /// Indicates whether the [`Sender`] is still connected.
    pub fn is_connected(&self) -> bool {
        let state = self.channel().state.load(Ordering::Relaxed);
        state::sender_alive(state)
    }

    /// Indicates whether this [`Receiver`] receives its
    /// message from the given [`Sender`].
    #[inline]
    pub fn receives_from(&self, sender: &Sender<T>) -> bool {
        self.channel == sender.channel
    }

    fn register_waker(&mut self, waker: &Waker) -> bool {
        let channel = self.channel();
        let mut receiver = channel.receiver.lock();

        // If we're already storing a waker for the same task,
        // we have nothing to do.
        if let Some(receiver) = &*receiver {
            if receiver.will_wake(waker) {
                return false;
            }
        }

        // When we don't, update the cached waker with the given one.
        *receiver = Some(waker.clone());

        true
    }

    /// Attempts to receive a value from this channel.
    pub fn try_recv(&mut self) -> Result<T, TryRecvError> {
        let channel = self.channel();

        // Clear the slot status as we're about to read it.
        let old = channel
            .state
            .fetch_and(!state::SLOT_FILLED, Ordering::AcqRel);

        // See if a value is available or return an appropriate error.
        if state::slot_filled(old) {
            // SAFETY: We have exclusive access to the value.
            Ok(unsafe { channel.value.get().read().assume_init() })
        } else if state::sender_alive(old) {
            Err(TryRecvError::Empty)
        } else {
            Err(TryRecvError::Disconnected)
        }
    }
}

impl<T> fmt::Debug for Receiver<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Receiver").finish()
    }
}

impl<T> Future for Receiver<T> {
    type Output = Result<T, RecvError>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut std::task::Context<'_>) -> Poll<Self::Output> {
        match self.try_recv() {
            Ok(value) => Poll::Ready(Ok(value)),
            Err(TryRecvError::Disconnected) => Poll::Ready(Err(RecvError)),
            Err(TryRecvError::Empty) => {
                // We didn't get a value yet, so set up the waker
                // for notifications from the Sender.
                if !self.register_waker(cx.waker()) {
                    // A waker is already configured for the task.
                    return Poll::Pending;
                }

                // In the unfortunate case where the sender sent a value
                // between the `try_recv` poll and the registration of a
                // waker, we would be out of luck and never get woken up.
                // So explicitly check that this is not the case.
                match self.try_recv() {
                    Ok(value) => Poll::Ready(Ok(value)),
                    Err(TryRecvError::Empty) => Poll::Pending,
                    Err(TryRecvError::Disconnected) => Poll::Ready(Err(RecvError)),
                }
            }
        }
    }
}

impl<T> Drop for Receiver<T> {
    fn drop(&mut self) {
        // Mark the Receiver as dropped.
        let old = self
            .channel()
            .state
            .fetch_and(!state::RECEIVER_ALIVE, Ordering::AcqRel);

        // If the Sender is not around anymore, we have to free the memory.
        // SAFETY: `Channel`s are originally allocated as `Box`es.
        if !state::sender_alive(old) {
            unsafe { drop(Box::from_raw(self.channel.as_ptr())) }
        }
    }
}

// SAFETY: `Receiver`s require mutable references for manipulation
// of their internal state, they are thus safe to share across
// thread boundaries as long as their value types are.
unsafe impl<T: Send> Send for Receiver<T> {}
unsafe impl<T: Send> Sync for Receiver<T> {}

impl<T> Unpin for Receiver<T> {}
