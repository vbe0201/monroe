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
//! Inspiration is drawn from previous works by
//! [kprotty](https://github.com/kprotty).
//!
//! [`monroe`]: ../monroe/index.html

#![feature(cfg_sanitize, ptr_const_cast)]

use std::{
    fmt, hint,
    ptr::NonNull,
    sync::atomic::{fence, AtomicUsize, Ordering},
};

mod cache;

mod error;
pub use self::error::*;

pub mod oneshot;

mod parker;
use self::parker::Parker;

mod queue;
use self::queue::Queue;

// Spin helpers are only needed on x86 and x86_64.
#[allow(unused)]
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

    // Initial refcount represents one receiver and one sender.
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

        // SAFETY: `Box` allocations are non-null.
        unsafe { NonNull::new_unchecked(Box::into_raw(alloc)) }
    }
}

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
/// Panics when `capacity` is 0 or exceeds [`isize::MAX`] when
/// rounded up to the nearest power of two.
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
        // SAFETY: `self.channel` is a valid `Box` allocation
        // and the constrained `&self` lifetime cannot make it
        // outlive its dropping point.
        unsafe { self.channel.as_ref() }
    }

    /// Returns the actual buffer capacity for messages in
    /// this channel.
    #[inline]
    pub fn capacity(&self) -> usize {
        self.channel().queue.capacity()
    }

    /// Indicates whether the [`Receiver`] is still connected.
    pub fn is_connected(&self) -> bool {
        let rc = self.channel().refcount.load(Ordering::Relaxed);
        refcount::has_receiver(rc)
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
        // Check if the receiving half is still alive,
        // no point in sending otherwise.
        if !self.is_connected() {
            return Err(TrySendError::Disconnected(value));
        }

        // Attempt to push the value into the queue if
        // unoccupied slots are currently available.
        if let Err(value) = self.channel().queue.try_push(value) {
            return Err(TrySendError::Full(value));
        }

        // Unpark a receiver task waiting for a value.
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
            // Try to send the value. If it succeeds immediately
            // or if the receiving half is disconnected, we have
            // nothing else to do.
            value = match self.try_send(value) {
                Ok(()) => return Ok(()),
                Err(TrySendError::Full(value)) => value,
                Err(TrySendError::Disconnected(value)) => return Err(SendError(value)),
            };

            let should_park = || {
                let can_push = self.channel().queue.can_push();
                self.is_connected() && !can_push
            };

            // We did not succeed in sending the value, attempt
            // to spin very briefly in hopes that a slot in the
            // Channel becomes unoccupied in the meantime.
            hint::spin_loop();

            // In case the receiver is still alive but we didn't
            // get blessed with a free slot off the bat, we have
            // to park this task waiting for the receiver to
            // unpark it when freeing a slot in the queue.
            self.channel().senders.park_one(should_park).await;
        }
    }
}

/// # Panics
///
/// Panics in the unlikely event that the user manages
/// to exhaust the upper limit of `usize::MAX >> 1`
/// concurrent [`Sender`] instances.
///
/// No realistic use case should ever hit this limit.
impl<T> Clone for Sender<T> {
    fn clone(&self) -> Self {
        // As per Boost documentation:
        //
        // > Increasing the reference counter can always be done with
        // > memory_order_relaxed: New references to an object can only
        // > be formed from an existing reference, and passing an existing
        // > reference from one thread to another must already provide
        // > any required synchronization.
        //
        // Since calling clone requires knowledge of the reference, that
        // alone is sufficient to prevent other threads from erroneously
        // deleting the object.
        let rc = self.channel().refcount.fetch_add(1, Ordering::Relaxed);

        // Sanity check that the user is not going nuts.
        assert_ne!(refcount::senders(rc), refcount::senders(usize::MAX));

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
        // Since `fetch_sub` is already atomic, we do not need to synchronize
        // with other threads unless we are going to delete the object.
        let rc = self.channel().refcount.fetch_sub(1, Ordering::Release);

        // If we're not the only remaining sender left, we have nothing to do.
        if refcount::senders(rc) != 1 {
            return;
        }

        // As the last sender, wake the receiver if it still exists.
        if refcount::has_receiver(rc) {
            self.channel().receiver.unpark_one();
            return;
        }

        // As per Boost documentation:
        //
        // > It is important to enforce any possible access to the object in one
        // > thread (through an existing reference) to *happen before* deleting
        // > the object in a different thread. This is achieved by a release
        // > operation after dropping a reference (any access to the object through
        // > this reference must obviously happen before), and an acquire operation
        // > before deleting the object.
        fence!(self.channel().refcount, Ordering::Acquire);

        // With the fence constructed, we can free the channel memory.
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
        // SAFETY: `self.channel` is a valid `Box` allocation
        // and the constrained `&self` lifetime cannot make it
        // outlive its dropping point.
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
    ///
    /// # Panics
    ///
    /// Panics in the unlikely event that the user manages
    /// to exhaust the upper limit of `usize::MAX >> 1`
    /// concurrent [`Sender`] instances.
    ///
    /// No realistic use case should ever hit this limit.
    pub fn make_sender(&self) -> Sender<T> {
        // As per Boost documentation:
        //
        // > Increasing the reference counter can always be done with
        // > memory_order_relaxed: New references to an object can only
        // > be formed from an existing reference, and passing an existing
        // > reference from one thread to another must already provide
        // > any required synchronization.
        //
        // Since calling clone requires knowledge of the reference, that
        // alone is sufficient to prevent other threads from erroneously
        // deleting the object.
        let rc = self.channel().refcount.fetch_add(1, Ordering::Relaxed);

        // Sanity check that the user is not going nuts.
        assert_ne!(refcount::senders(rc), refcount::senders(usize::MAX));

        Sender {
            channel: self.channel,
        }
    }

    /// Attempts to receive a value from this channel.
    pub fn try_recv(&mut self) -> Result<T, TryRecvError> {
        // Attempt to pop a value off the queue.
        // SAFETY: Mutable reference ensures exclusivity.
        match unsafe { self.channel().queue.try_pop() } {
            Some(value) => {
                // We succeeded in getting a value and freeing one slot
                // in the queue, wake one parked sender waiting to push
                // the next value.
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
            // Try to receive a value. If it succeeds immediately
            // or if the senders are disconnected, we have nothing
            // else to do.
            match self.try_recv() {
                Ok(value) => return Ok(value),
                Err(TryRecvError::Disconnected) => return Err(RecvError),
                Err(TryRecvError::Empty) => {}
            }

            let should_park = || {
                // SAFETY: Mutable reference ensures exclusivity.
                let can_pop = unsafe { self.channel().queue.can_pop() };
                self.is_connected() && !can_pop
            };

            // We did not succeed in receiving a value, attempt
            // to spin very briefly in hopes that a slot in the
            // Channel gets filled in the meantime.
            hint::spin_loop();

            // In case at least one sender is still alive and
            // we didn't get blessed with a provided value to
            // take off the bat, we have to park this task
            // waiting for a sender to unpark it when pushing.
            self.channel().receiver.park_one(should_park).await;
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
        use refcount::RECEIVER_ALIVE;

        // Since `fetch_sub` is already atomic, we do not need to synchronize
        // with other tasks unless we are going to delete the object.
        let rc = self
            .channel()
            .refcount
            .fetch_and(!RECEIVER_ALIVE, Ordering::Release);

        // If senders are still alive, unpark all of them.
        if refcount::senders(rc) > 0 {
            self.channel().senders.unpark_all();
            return;
        }

        // As per Boost documentation:
        //
        // > It is important to enforce any possible access to the object in one
        // > thread (through an existing reference) to *happen before* deleting
        // > the object in a different thread. This is achieved by a release
        // > operation after dropping a reference (any access to the object through
        // > this reference must obviously happen before), and an acquire operation
        // > before deleting the object.
        fence!(self.channel().refcount, Ordering::Acquire);

        // With the fence constructed, we can free the channel memory.
        // SAFETY: `Channel`s are originally allocated as `Box`es.
        unsafe { drop(Box::from_raw(self.channel.as_ptr())) }
    }
}

// SAFETY: `Receiver`s require mutable references for manipulation
// of their internal state, they are thus safe to share across
// thread boundaries as long as their value types are.
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
    /// Consumes the ID object, returning its integer value.
    #[inline]
    pub fn value(self) -> usize {
        self.0
    }
}
