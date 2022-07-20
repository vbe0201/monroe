use monroe_inbox::Receiver;
pub use monroe_inbox::{Id, RecvError, TryRecvError};

use crate::actor::Actor;

/// The execution context for an [`Actor`].
///
/// Each actor individually manages its own context
/// instance it operates on.
pub struct Context<A: Actor> {
    mailbox: Receiver<A::Message>,
}

impl<A: Actor> Context<A> {
    /// Gets the buffer capacity for messages in the
    /// associated [`Actor`]'s mailbox.
    pub fn mailbox_capacity(&self) -> usize {
        self.mailbox.capacity()
    }

    /// Indicates whether any outstanding addresses (TODO: Doc link)
    /// are still alive for this actor.
    pub fn is_connected(&self) -> bool {
        self.mailbox.is_connected()
    }

    /// Gets a unique [`Id`] of the associated [`Actor`].
    ///
    /// Note that uniqueness of the value is only guaranteed
    /// for the lifetime of the actor.
    ///
    /// When this actor terminates and a different one is
    /// started, it may reuse this actor's ID value.
    pub fn id(&self) -> Id {
        self.mailbox.id()
    }

    /// Attempts to receive the next [`Actor::Message`] from
    /// the mailbox.
    ///
    /// This may fail when all addresses (TODO: doc link) are
    /// dropped or when the mailbox currently does not buffer
    /// any outstanding messages.
    pub fn try_recv_next(&mut self) -> Result<A::Message, TryRecvError> {
        self.mailbox.try_recv()
    }

    /// Attempts to receive the next [`Actor::Message`]
    /// asynchronously.
    ///
    /// Unlike [`Context::try_recv_next`], this will wait for
    /// a message to become available if the channel is empty.
    ///
    /// The operation may fail when all addresses (TODO: doc link)
    /// are dropped.
    pub async fn recv_next(&mut self) -> Result<A::Message, RecvError> {
        self.mailbox.recv().await
    }
}
