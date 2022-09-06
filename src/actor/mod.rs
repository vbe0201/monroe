use std::future::Future;

use crate::{context::Context, runtime::RuntimeHandle};

mod message;
pub use self::message::*;

mod new;
pub use self::new::*;

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
/// # Message Passing
///
/// Since actors are isolated to protect their state from the
/// outer world, all communication happens through
/// *message passing* as a synchronization mechanism.
///
/// [`Address`]es are handles that reference an actor and can
/// be used to send messages to it from outside.
///
/// A program may use many references to the same actor
/// concurrently.
///
/// An actor can obtain its own [`Address`] using
/// [`Context::address`].
///
/// Actors then poll pending [`Actor::Message`] notifications
/// from their execution [`Context`] in order to process them.
///
/// If actors should process multiple types of messages, it is
/// recommended to wrap all types up in a single enum.
///
/// TODO: tell/ask strategies.
///
/// # Supervision
///
/// TODO
///
/// [`Address`]: crate::address::Address
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

    /// A handle to the underlying runtime that will be
    /// used for the actor.
    ///
    /// This is necessary for the [`Context`] to provide
    /// the actor with access handle to manipulate its
    /// runtime behavior.
    type RuntimeHandle: RuntimeHandle;

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
