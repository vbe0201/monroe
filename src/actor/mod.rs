use std::future::Future;

mod message;
pub use self::message::*;

/// An encapsulated unit of computation.
///
/// Actors are finite state machines which communicate merely
/// by exchanging messages. They don't share their state with
/// the outer world which removes the need for synchronization
/// primitives in concurrent programs.
///
/// # Message Passing
///
/// TODO
///
/// # Supervision
///
/// TODO
pub trait Actor: Sized + Send + 'static {
    /// The message type processed by this actor.
    type Message: Message;

    /// The error type produced by this actor on failure.
    ///
    /// It is valid to set this to [never type] (`!`) if
    /// the actor never produces errors.
    ///
    /// [never type]: https://doc.rust-lang.org/std/primitive.never.html
    type Error;

    /// The [`Future`] type produced by [`Actor::run`].
    type Fut<'a>: Future<Output = ()> + Send + 'a
    where
        Self: 'a;

    /// Executes the actor until it terminates by itself.
    ///
    /// This method is repeatedly called whenever an actor
    /// is *started* and also *restarted*.
    ///
    /// It models the entire lifecycle of an actor and does
    /// not preserve its state from previous "rounds" of
    /// execution into new ones. TODO: Supervisor link.
    fn run<'a>(&'a mut self /* TODO: Context */) -> Self::Fut<'a>;
}
