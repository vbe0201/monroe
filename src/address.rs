use std::fmt;

use monroe_inbox::{oneshot, Sender};
pub use monroe_inbox::{Id, SendError, TrySendError};

use crate::actor::{Actor, Ask, Message};

/// A strong reference to a local [`Actor`].
///
/// [`Address`]es serve as handles to isolated actor entities
/// and provide the functionality to send messages to the
/// referenced [`Actor`]s.
///
/// While message delivery for local actors is guaranteed, no
/// guarantees are made about the order of delivery.
///
/// Values of this type are strongly reference-counted. An
/// [`Actor`] will be able to receive messages until the last
/// [`Address`] reference is dropped.
///
/// Please note that the existence of an [`Address`] *DOES NOT*
/// automatically guarantee that the referenced actor `A` is
/// running -- it may still crash or terminate from its own
/// circumstances, which makes the methods for communicating
/// with the actor fallible.
///
/// When more references are needed, this type can be cheaply
/// cloned and passed by value.
///
/// [`Actor`]s may use [`Context::address`] to retrieve their
/// own reference handles.
///
/// [`Context::address`]: crate::context::Context::address
pub struct Address<A: Actor> {
    tx: Sender<A::Message>,
}

impl<A: Actor> Address<A> {
    pub(crate) const fn new(tx: Sender<A::Message>) -> Self {
        Self { tx }
    }

    /// Gets a unique [`Id`] of the referenced [`Actor`].
    ///
    /// Note that uniqueness of the value is only guaranteed
    /// for the lifetime of the actor.
    ///
    /// When this actor terminates and a different one is
    /// started, it may reuse this actor's ID value.
    pub fn id(&self) -> Id {
        self.tx.id()
    }

    /// Indicates whether the referenced [`Actor`] is still
    /// alive.
    ///
    /// The return value of this method is representative
    /// for any and all [`Address`]es for the same actor.
    pub fn is_connected(&self) -> bool {
        self.tx.is_connected()
    }

    /// Attempts to *tell* a given message to the referenced
    /// [`Actor`]'s mailbox.
    ///
    /// This will return immediately after the message was
    /// sent or an error was encountered, not waiting for
    /// the actor to actually receive and/or process it.
    ///
    /// This will fail if the actor has already terminated
    /// or if the mailbox is currently full.
    ///
    /// On error, ownership of the sent message is transferred
    /// back to the caller.
    pub fn try_tell<M: Message>(&self, msg: M) -> Result<(), TrySendError<A::Message>>
    where
        M: Into<A::Message>,
    {
        self.tx.try_send(msg.into())
    }

    /// *Tells* a given message to the referenced [`Actor`]'s
    /// mailbox asynchronously.
    ///
    /// This will return immediately after the message was
    /// sent or an error was encountered, not waiting for
    /// the actor to actually receive and/or process it.
    ///
    /// This will fail if the actor has already terminated.
    /// Unlike [`Address::try_tell`], this will wait for a
    /// slot in the mailbox to become free if it is full.
    ///
    /// On error, ownership of the sent message is transferred
    /// back to the caller.
    pub async fn tell<M: Message>(&self, msg: M) -> Result<(), SendError<A::Message>>
    where
        M: Into<A::Message>,
    {
        self.tx.send(msg.into()).await
    }

    /// Attempts to *ask* the referenced [`Actor`] to process
    /// a given message `Req`.
    ///
    /// This will then wait for the actor to receive and handle
    /// the message, returning the result (`Res`) of the
    /// operation.
    ///
    /// This will fail if the actor has already terminated or
    /// if the mailbox is currently full.
    ///
    /// On error. ownership of the sent message is transferred
    /// back to the caller.
    pub async fn try_ask<Req: Message, Res: Message>(
        &self,
        msg: Req,
    ) -> Result<Res, TrySendError<A::Message>>
    where
        Ask<Req, Res>: Into<A::Message>,
    {
        let (tx, rx) = oneshot::channel();
        match self.tx.try_send(Ask::new(msg, tx).into()) {
            Ok(()) => match rx.await {
                Ok(res) => Ok(res),
                Err(_) => request_dropped(),
            },
            Err(e) => Err(e),
        }
    }

    /// *Asks* the referenced [`Actor`] to process a given
    /// message `Req`.
    ///
    /// This will then wait for the actor to receive and handle
    /// the message, returning the result (`Res`) of the
    /// operation.
    ///
    /// This will fail if the actor has already terminated.
    /// Unlike [`Address::try_ask`], this will wait for a
    /// slot in the mailbox to become free if it is full.
    ///
    /// On error, ownership of the sent message is transferred
    /// back to the caller.
    pub async fn ask<Req: Message, Res: Message>(
        &self,
        msg: Req,
    ) -> Result<Res, SendError<A::Message>>
    where
        Ask<Req, Res>: Into<A::Message>,
    {
        let (tx, rx) = oneshot::channel();
        match self.tx.send(Ask::new(msg, tx).into()).await {
            Ok(()) => match rx.await {
                Ok(res) => Ok(res),
                Err(_) => request_dropped(),
            },
            Err(e) => Err(e),
        }
    }
}

impl<A: Actor> Clone for Address<A> {
    fn clone(&self) -> Self {
        Self {
            tx: self.tx.clone(),
        }
    }
}

impl<A: Actor> fmt::Debug for Address<A> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Address").finish()
    }
}

impl<A: Actor> PartialEq for Address<A> {
    fn eq(&self, other: &Self) -> bool {
        self.tx.same_channel(&other.tx)
    }
}

#[cold]
fn request_dropped() -> ! {
    panic!("An `Ask` request was dropped without calling `Ask::respond`. This is a logic bug!")
}
