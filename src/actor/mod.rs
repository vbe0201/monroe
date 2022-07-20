use std::future::Future;

use crate::context::Context;

mod message;
pub use self::message::*;

/// An encapsulated unit of computation.
///
/// Actors are finite state machines which communicate merely
/// by exchanging messages. They don't share their state with
/// the outer world which removes the need for synchronization
/// primitives in concurrent programs.
///
/// # Lifecycle
///
/// The entire lifecycle of an actor is modeled in
/// [`Actor::run`].
///
/// This method will be responsible for processing any and all
/// messages and gets exclusive access to the actor's inner
/// state through the mutable `self` reference.
///
/// It is also handed the actor's execution [`Context`] which
/// allows an actor to customize its runtime behavior.
///
/// TODO: Document errors and supervision behavior.
///
/// # Addresses
///
/// TODO
///
/// # Message Passing
///
/// Actors poll pending [`Actor::Message`] notifications from
/// their execution [`Context`] in order to process them.
///
/// TODO: tell/ask strategies.
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
    type Fut<'a>: Future<Output = Result<(), Self::Error>> + Send + 'a
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
    fn run<'a>(&'a mut self, ctx: &'a mut Context<Self>) -> Self::Fut<'a>;
}
