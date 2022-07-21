//! Implementation of [`Actor`] supervision.
//! 
//! At its core, this module provides the [`Supervisor`]
//! trait which allow for defining customized restarting
//! behavior when the supervised [`Actor`] fails or crashes.
//! 
//! This is achieved using [`ActorFate`] values which then
//! influence how new actor state will be produced by its
//! associated [`NewActor`] factory.

use std::any::Any;

use crate::actor::{Actor, NewActor};

/// The fate of a faulting actor, chosen by its [`Supervisor`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum ActorFate<Arg> {
    /// The actor should be restarted, with a user-provided
    /// argument for [`NewActor::make`].
    Restart(Arg),
    /// The actor should be permanently stopped.
    Stop,
}

/// A supervisor that oversees the execution of an
/// [`Actor`][crate::actor::Actor].
///
/// In order to build self-healing systems, every actor in
/// `monroe` must have a designated supervisor which deals
/// with any kinds of errors and crashes an actor by itself
/// cannot recover from.
pub trait Supervisor<NA: NewActor>: Send + 'static {
    /// Called when an actor faults by returning
    /// [`Actor::Error`] from [`Actor::run`].
    fn on_error(&mut self, error: <NA::Actor as Actor>::Error) -> ActorFate<NA::Arg>;

    /// Called when restarting an actor failed for the
    /// *first* time.
    ///
    /// This is the case when [`NewActor::make`] returned
    /// an error.
    ///
    /// Refer to [`Supervisor::on_second_restart_error`]
    /// for details on how actors which fail to be restarted
    /// for the *second* time are dealt with.
    fn on_restart_error(&mut self, error: NA::Error) -> ActorFate<NA::Arg>;

    /// Called when restarting an actor failed for the
    /// *second* time.
    ///
    /// This is the case when [`NewActor::make`] returned
    /// an error even after a prior call to
    /// [`Supervisor::on_restart_error`].
    ///
    /// The error is only provided for debugging purposes;
    /// when this method is ever called, the actor will
    /// be unconditionally terminated.
    ///
    /// This measure is in place to protect against endless
    /// cycles of failing [`NewActor::make`] call with the
    /// [`Supervisor`] still issuing more attempts.
    fn on_second_restart_error(&mut self, error: NA::Error);

    /// Called when an actor crashed due to a panic from
    /// inside [`Actor::run`].
    ///
    /// The default implementation will stop the actor
    /// unconditionally as it is generally harder to recover
    /// from such a state of metastability than from normal
    /// execution errors.
    fn on_panic(&mut self, panic: Box<dyn Any + Send + 'static>) -> ActorFate<NA::Arg> {
        drop(panic);
        ActorFate::Stop
    }
}

// TODO: Implementations for closures for more ergonomics.

/// A general-purpose [`Supervisor`] implementation that
/// never restarts a failed actor.
#[derive(Debug)]
pub struct NoRestart;

// TODO: Error logging?
impl<NA: NewActor> Supervisor<NA> for NoRestart {
    fn on_error(
        &mut self,
        _error: <NA::Actor as Actor>::Error,
    ) -> ActorFate<<NA as NewActor>::Arg> {
        ActorFate::Stop
    }

    fn on_restart_error(
        &mut self,
        _error: <NA as NewActor>::Error,
    ) -> ActorFate<<NA as NewActor>::Arg> {
        ActorFate::Stop
    }

    fn on_second_restart_error(&mut self, _error: <NA as NewActor>::Error) {}
}
