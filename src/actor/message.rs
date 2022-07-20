use std::{fmt, future::Future};

use monroe_inbox::oneshot;

/// A marker trait that represents an [`Actor`] message.
///
/// Every message is supposed to be an owned, sized type
/// that can safely be shared across thread boundaries.
///
/// This enables static validation that actors do not share
/// their exclusive state with the outer world, as intended
/// by the actor model.
///
/// This trait is implemented for eligible types by default,
/// so crate users should practically never have to do this.
///
/// [`Actor`]: super::Actor
pub trait Message: Sized + Send + 'static {}

impl<T: Sized + Send + 'static> Message for T {}

/// A message request *asked* at an [`Actor`][super::Actor].
///
/// This type carries a [`Message`] type `Req` to its designated
/// actor and forces the actor to produce a `Res` object after
/// processing the `Req` message payload.
///
/// The `Res` can then be sent back to the *asking* actor, which
/// is made to wait for the time the request processing takes on
/// the destination actor.
///
/// Users are not supposed to create their own instances except
/// for the purposes explained at [`Ask::cut`].
///
/// Instead, the convenience methods [`Address::try_ask`] and
/// [`Address::ask`] should be used instead.
///
/// [`Address::try_ask`]: crate::address::Address::try_ask
/// [`Address::ask`]: crate::address::Address::ask
#[must_use = "`Ask::respond` must be called to produce the required response"]
pub struct Ask<Req, Res> {
    msg: Req,
    tx: oneshot::Sender<Res>,
}

impl<Req, Res> Ask<Req, Res> {
    pub(crate) const fn new(msg: Req, tx: oneshot::Sender<Res>) -> Self {
        Self { msg, tx }
    }

    /// Consumes the *ask* request and returns the inner
    /// message payload.
    ///
    /// It is discouraged to use this method in actors as
    /// it eliminates the possibility to respond using
    /// [`Ask::respond`].
    pub fn into_inner(self) -> Req {
        self.msg
    }

    /// Creates a **cut** ask request with a given message
    /// payload.
    ///
    /// In the event where the response to an *ask* request
    /// might not be needed, it can save time to *tell* it
    /// to the actor instead of making it wait for the
    /// response first just to discard it.
    ///
    /// This method aids in creating an [`Ask`] request which
    /// cannot receive a response. It may be passed to
    /// [`Address::try_tell`] or [`Address::tell`].
    ///
    /// [`Address::try_tell`]: crate::address::Address::try_tell
    /// [`Address::tell`]: crate::address::Address::tell
    pub fn cut(msg: Req) -> Self {
        // We don't need the Receiver as we're not listening for responses.
        let (tx, _) = oneshot::channel();

        Self { msg, tx }
    }
}

impl<Req: Message, Res: Message> Ask<Req, Res> {
    /// Responds to the request by executing the supplied
    /// closure and sending back its result.
    ///
    /// The asynchronous closure takes the `Req` message
    /// payload by value and produces a `Res` result with
    /// an arbitrary error.
    ///
    /// This is the preferred way of handling [`Ask`]
    /// messages on the actor.
    pub async fn respond<F, Fut, Err>(self, f: F) -> Result<(), Err>
    where
        F: FnOnce(Req) -> Fut,
        Fut: Future<Output = Result<Res, Err>>,
    {
        let Self { msg, tx } = self;

        // We don't care if the other side is listening.
        let _ = tx.send(f(msg).await?);

        Ok(())
    }
}

impl<Req: fmt::Debug, Res> fmt::Debug for Ask<Req, Res> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Ask").field("msg", &self.msg).finish()
    }
}
