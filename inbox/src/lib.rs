//! Bounded MPSC channels with backpressure for use in the
//! [`monroe`] actor system.
//!
//! The [`channel`] function is used to create a pair of
//! [`Sender`] and [`Receiver`] over a shared channel.
//!
//! [`Sender`]s can be duplicated and shared across tasks
//! ("multi producer") but they only publish to a single,
//! shared [`Receiver`] ("single consumer").
//!
//! These channels are solely designed for use in asynchronous
//! tasks, so no synchronous APIs for sending and receiving
//! values are provided.
//!
//! [`monroe`]: ../monroe/

#![feature(cfg_sanitize)]

use std::{
    fmt, hint,
    ptr::NonNull,
    sync::atomic::{fence, AtomicUsize, Ordering},
};

mod cache;

mod error;
pub use self::error::*;

mod parker;
use self::parker::Parker;

mod queue;
use self::queue::Queue;

mod spin;

#[cfg(not(sanitize = "thread"))]
macro_rules! fence {
    ($x:expr, $order:expr) => {
        fence($order)
    };
}

// ThreadSanitizer does not support memory fences. To avoid false
// positive reports, we use atomic loads for synchronization.
#[cfg(sanitize = "thread")]
macro_rules! fence {
    ($x:expr, $order:expr) => {
        $x.load($order)
    };
}

mod refcount {
    // Bit in the channel refcount to mark the receiver alive.
    pub const RECEIVER_ALIVE: usize = 1 << (usize::BITS - 1);

    #[inline(always)]
    pub const fn initial() -> usize {
        RECEIVER_ALIVE | 1
    }

    #[inline(always)]
    pub const fn has_receiver(refcount: usize) -> bool {
        refcount & RECEIVER_ALIVE != 0
    }

    #[inline(always)]
    pub const fn senders(refcount: usize) -> usize {
        refcount & !RECEIVER_ALIVE
    }
}

struct Channel<T> {
    queue: Queue<T>,
    refcount: AtomicUsize,
    senders: Parker,
    receiver: Parker,
}

impl<T> Channel<T> {
    fn alloc(capacity: usize) -> NonNull<Self> {
        let alloc = Box::new(Self {
            queue: Queue::new(capacity),
            refcount: AtomicUsize::new(refcount::initial()),
            senders: Parker::new(),
            receiver: Parker::new(),
        });

        unsafe { NonNull::new_unchecked(Box::into_raw(alloc)) }
    }
}

unsafe impl<T: Send> Send for Channel<T> {}
unsafe impl<T> Sync for Channel<T> {}

/// Creates a new MPSC channel and returns both halves of it for
/// communication between asynchronous tasks.
///
/// The channel will buffer up to the provided capacity of
/// messages, rounded up to the nearest power of two.
///
/// All data sent on the [`Sender`] will become available to
/// the [`Receiver`] in the same order as it was sent.
///
/// [`Sender`]s can be cheaply cloned and sent between tasks,
/// whereas only one [`Receiver`] is supported.
///
/// # Panics
///
/// Panics when `capacity` is 0.
pub fn channel<T>(capacity: usize) -> (Sender<T>, Receiver<T>) {
    assert_ne!(capacity, 0, "capacity of 0 is not allowed!");

    let channel = Channel::alloc(capacity);
    (Sender { channel }, Receiver { channel })
}

/// The sending half of a channel which makes `T` values
/// available to the [`Receiver`].
///
/// New instances are created using the [`channel`] function.
///
/// This type can be cheaply cloned and shared between tasks
/// if more senders to the same [`Receiver`] are desired.
pub struct Sender<T> {
    channel: NonNull<Channel<T>>,
}

impl<T> Sender<T> {
    #[inline]
    fn channel(&self) -> &Channel<T> {
        unsafe { self.channel.as_ref() }
    }

    /// Returns the actual buffer capacity for messages in
    /// this channel.
    #[inline]
    pub fn capacity(&self) -> usize {
        self.channel().queue.capacity()
    }

    /// Indicates whether the [`Receiver`] is still connected.
    #[inline]
    pub fn is_connected(&self) -> bool {
        let refcount = self.channel().refcount.load(Ordering::Relaxed);
        refcount::has_receiver(refcount)
    }

    /// Indicates whether another [`Sender`] sends to the same
    /// channel as this one.
    #[inline]
    pub fn same_channel(&self, other: &Self) -> bool {
        self.channel == other.channel
    }

    /// Indicates whether the given [`Receiver`] receives its
    /// messages from this [`Sender`].
    #[inline]
    pub fn sends_to(&self, receiver: &Receiver<T>) -> bool {
        self.channel == receiver.channel
    }

    /// Gets a unique [`Id`] for the lifetime of the underlying
    /// *channel*.
    ///
    /// See the documentation of [`Id`] for more details.
    #[inline]
    pub fn id(&self) -> Id {
        Id(self.channel.as_ptr() as usize)
    }

    /// Attempts to send the `value` into this channel.
    ///
    /// On error, ownership of that value is transferred back
    /// in [`TrySendError`].
    pub fn try_send(&self, value: T) -> Result<(), TrySendError<T>> {
        if !self.is_connected() {
            return Err(TrySendError::Disconnected(value));
        }

        if let Err(value) = self.channel().queue.try_push(value) {
            return Err(TrySendError::Full(value));
        }
        self.channel().receiver.unpark_one();

        Ok(())
    }

    /// Sends the `value` asynchronously, waiting for a slot to
    /// become available if the channel is full.
    ///
    /// On error, ownership of that value is transferred back
    /// in [`SendError`].
    pub async fn send(&self, mut value: T) -> Result<(), SendError<T>> {
        loop {
            value = match self.try_send(value) {
                Ok(()) => return Ok(()),
                Err(TrySendError::Full(value)) => value,
                Err(TrySendError::Disconnected(value)) => return Err(SendError(value)),
            };

            let should_park = || {
                let can_push = self.channel().queue.can_push();
                self.is_connected() && !can_push
            };

            hint::spin_loop();
            self.channel().senders.park(should_park).await;
        }
    }
}

impl<T> Clone for Sender<T> {
    fn clone(&self) -> Self {
        let old_ref_count = self.channel().refcount.fetch_add(1, Ordering::Relaxed);
        assert_ne!(
            refcount::senders(old_ref_count),
            refcount::senders(usize::MAX),
        );

        Self {
            channel: self.channel,
        }
    }
}

impl<T> fmt::Debug for Sender<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Sender").finish()
    }
}

impl<T> Drop for Sender<T> {
    fn drop(&mut self) {
        let old_refcount = self.channel().refcount.fetch_sub(1, Ordering::Release);
        if refcount::senders(old_refcount) != 1 {
            return;
        }

        if refcount::has_receiver(old_refcount) {
            self.channel().receiver.unpark_one();
        } else {
            // We are the last sender and no receiver is alive.
            // Deallocate the channel memory.

            fence!(self.channel().refcount, Ordering::Acquire);

            unsafe { drop(Box::from_raw(self.channel.as_ptr())) }
        }
    }
}

unsafe impl<T: Send> Send for Sender<T> {}
unsafe impl<T: Send> Sync for Sender<T> {}

impl<T> Unpin for Sender<T> {}

/// The receiving half of a channel which obtains `T` values
/// from [`Sender`]s in the order they were sent.
///
/// New instances are created using the [`channel`] function.
pub struct Receiver<T> {
    channel: NonNull<Channel<T>>,
}

impl<T> Receiver<T> {
    #[inline]
    fn channel(&self) -> &Channel<T> {
        unsafe { self.channel.as_ref() }
    }

    /// Returns the actual buffer capacity for messages in
    /// this channel.
    #[inline]
    pub fn capacity(&self) -> usize {
        self.channel().queue.capacity()
    }

    /// Indicates whether at least one [`Sender`] is still
    /// connected.
    #[inline]
    pub fn is_connected(&self) -> bool {
        let refcount = self.channel().refcount.load(Ordering::Relaxed);
        refcount::senders(refcount) > 0
    }

    /// Indicates whether this [`Receiver`] receives its
    /// messages from the given [`Sender`].
    #[inline]
    pub fn receives_from(&self, sender: &Sender<T>) -> bool {
        self.channel == sender.channel
    }

    /// Gets a unique [`Id`] for the lifetime of the underlying
    /// *channel*.
    ///
    /// See the documentation of [`Id`] for more details.
    #[inline]
    pub fn id(&self) -> Id {
        Id(self.channel.as_ptr() as usize)
    }

    /// Creates a new [`Sender`] object for this receiver.
    ///
    /// This method can still be used after the initially
    /// obtained [`Sender`] and all its clones were dropped.
    #[inline]
    pub fn make_sender(&self) -> Sender<T> {
        let old_ref_count = self.channel().refcount.fetch_add(1, Ordering::Relaxed);
        assert_ne!(
            refcount::senders(old_ref_count),
            refcount::senders(usize::MAX),
        );

        Sender {
            channel: self.channel,
        }
    }

    /// Attempts to receive a value from this channel.
    pub fn try_recv(&mut self) -> Result<T, TryRecvError> {
        match unsafe { self.channel().queue.try_pop() } {
            Some(value) => {
                self.channel().senders.unpark_one();
                Ok(value)
            }
            None if !self.is_connected() => Err(TryRecvError::Disconnected),
            None => Err(TryRecvError::Empty),
        }
    }

    /// Receives a value asynchronously, waiting for one to
    /// become available when the chanenl is empty.
    pub async fn recv(&mut self) -> Result<T, RecvError> {
        loop {
            match self.try_recv() {
                Ok(value) => return Ok(value),
                Err(TryRecvError::Disconnected) => return Err(RecvError),
                Err(TryRecvError::Empty) => {}
            }

            let should_park = || {
                let can_pop = unsafe { self.channel().queue.can_pop() };
                self.is_connected() && !can_pop
            };

            hint::spin_loop();
            self.channel().receiver.park(should_park).await;
        }
    }
}

impl<T> fmt::Debug for Receiver<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Receiver").finish()
    }
}

impl<T> Drop for Receiver<T> {
    fn drop(&mut self) {
        let old_refcount = self
            .channel()
            .refcount
            .fetch_and(!refcount::RECEIVER_ALIVE, Ordering::Release);

        if refcount::senders(old_refcount) != 0 {
            self.channel().senders.unpark_all();
        } else {
            // We are the only receiver and no senders are alive.
            // Deallocate the channel memory.

            fence!(self.channel().refcount, Ordering::Acquire);

            unsafe { drop(Box::from_raw(self.channel.as_ptr())) }
        }
    }
}

unsafe impl<T: Send> Send for Receiver<T> {}
unsafe impl<T: Send> Sync for Receiver<T> {}

impl<T> Unpin for Receiver<T> {}

/// A unique identifier of a *channel* for its lifetime.
///
/// Obtained by calling [`Sender::id`] or [`Receiver::id`],
/// they act as a unique identifiers for the whole channel
/// (match for all [`Sender`]s and [`Receiver`]s to the
/// same channel).
///
/// # Lifetime
///
/// An [`Id`] becomes invalid the moment all [`Sender`]s and
/// [`Receiver`]s to the same channel are dropped.
///
/// It should not be kept beyond this point.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Id(usize);

impl Id {
    /// Gets the integer value of this ID.
    #[inline]
    pub fn value(self) -> usize {
        self.0
    }
}
