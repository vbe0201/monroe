//! Runtime abstraction for spawning actor tasks.
//!
//! monroe's approach is to model actors as asynchronous tasks
//! spawned onto a runtime.
//!
//! In order to allow adopting this library to as many execution
//! environments as possible, we allow users to bring their own
//! runtimes as long as they satisfy the requirements.

use std::{
    any::Any,
    future::Future,
    marker::PhantomPinned,
    panic::{catch_unwind, AssertUnwindSafe},
    pin::{pin, Pin},
    ptr::NonNull,
    task::{self, Poll, Waker},
};

use crate::{
    actor::{Actor, NewActor},
    address::Address,
    context::Context,
    supervisor::{ActorFate, Supervisor},
};

#[cfg(feature = "tokio-rt")]
pub mod tokio;

/// An async runtime for monroe to spawn [`Actor`] futures.
pub trait Runtime: Sized + Send + Sync {
    /// A handle to the runtime.
    ///
    /// Instances of this type should provide the same
    /// functionality as direct access to the [`Runtime`],
    /// but with the addition that they can be arbitrarily
    /// cloned and shared around.
    type Handle: RuntimeHandle;

    /// Gets a [`Runtime::Handle`] that can be passed around.
    ///
    /// Allows for interacting with the runtime in the same
    /// way as having access to the runtime directly.
    fn handle(&self) -> Self::Handle;

    /// Runs a future to completion on the runtime.
    ///
    /// This represents the runtime's entrypoint and actors
    /// should only be spawned from within.
    fn block_on<Fn, F>(&self, f: Fn) -> F::Output
    where
        Fn: FnOnce(Self::Handle) -> F,
        F: Future;
}

/// An access handle to a [`Runtime`].
///
/// While [`Runtime`] itself only serves as an entrypoint to an
/// asynchronous application, handles are what actually enable
/// spawning actors, futures or shutting down altogether.
///
/// Actors have the handle for their runtime associated with
/// them and can request access to it through their context.
pub trait RuntimeHandle: Sized + Clone + Send + Sync {
    /// Spawns a [`Future`] onto the runtime.
    ///
    /// It will immediately begin to execute and cannot be
    /// manipulated or introspected from the outside.
    fn spawn<F>(&self, fut: F)
    where
        F: Future + Send + 'static,
        F::Output: Send + 'static;

    /// Aborts all tasks currently spawned on the runtime.
    ///
    /// Note that this may or may not await the cancellation
    /// of the tasks.
    fn stop(&self);

    /// Spawns an actor on the runtime given its [`Supervisor`],
    /// the [`NewActor`] factory, and a creation argument.
    ///
    /// See the [`NewActor`] documentation for details on how
    /// actual actor objects are managed and created.
    ///
    /// Returns an [`Address`] that references the actor on
    /// success, or an error on failure to create one.
    fn spawn_actor<S, NA, A>(
        &self,
        supervisor: S,
        new_actor: NA,
        arg: NA::Arg,
    ) -> Result<Address<NA::Actor>, NA::Error>
    where
        S: Supervisor<NA>,
        NA: NewActor<Actor = A>,
        A: Actor<RuntimeHandle = Self>,
    {
        // TODO: Arbitrary mailbox configurations?
        let (tx, rx) = monroe_inbox::channel(num_cpus::get() * 4);

        let address = Address::new(tx);
        let context = Context::new(rx, self.clone());

        let runner = ActorRunner::new(supervisor, new_actor, context, arg)?;
        self.spawn(runner.run());

        Ok(address)
    }
}

struct ActorRunner<S, NA: NewActor> {
    actor: NA::Actor,
    supervisor: S,
    new_actor: NA,
    context: Context<NA::Actor>,
}

impl<S: Supervisor<NA>, NA: NewActor> ActorRunner<S, NA> {
    fn new(
        supervisor: S,
        mut new_actor: NA,
        mut context: Context<NA::Actor>,
        arg: NA::Arg,
    ) -> Result<Self, NA::Error> {
        Ok(Self {
            actor: new_actor.make(&mut context, arg)?,
            supervisor,
            new_actor,
            context,
        })
    }

    unsafe fn create_new_actor(&mut self, arg: NA::Arg) -> Result<(), NA::Error> {
        // Create a new actor and replace the old object, dropping it in-place.
        self.new_actor.make(&mut self.context, arg).map(|actor| {
            // SAFETY: When pinning once, we guarantee that an object never moves.
            //
            // We can allow `Actor::run` to take a mutable reference as the object
            // is only pinned afterwards just to replace it with a new object.
            //
            // And since `.set()` doesn't return anything, we're never holding a
            // pinned reference to that new object. In turn, we are allowed to take
            // mutable references to that as well until we pin it again for drop.
            Pin::new_unchecked(&mut self.actor).set(actor);
        })
    }

    unsafe fn handle_restart_error(
        self: Pin<&mut Self>,
        waker: &Waker,
        error: NA::Error,
    ) -> Poll<()> {
        let this = self.get_unchecked_mut();
        match this.supervisor.on_restart_error(error) {
            ActorFate::Restart(arg) => match this.create_new_actor(arg) {
                Ok(()) => {
                    waker.wake_by_ref();
                    Poll::Pending
                }
                Err(error) => {
                    // Notify the supervisor that this actor is gone.
                    this.supervisor.on_second_restart_error(error);

                    Poll::Ready(())
                }
            },

            ActorFate::Stop => Poll::Ready(()),
        }
    }

    unsafe fn restart_actor(self: Pin<&mut Self>, waker: &Waker, arg: NA::Arg) -> Poll<()> {
        let this = self.get_unchecked_mut();
        match this.create_new_actor(arg) {
            Ok(()) => {
                waker.wake_by_ref();
                Poll::Pending
            }

            Err(error) => Pin::new_unchecked(this).handle_restart_error(waker, error),
        }
    }

    unsafe fn handle_stop(
        self: Pin<&mut Self>,
        waker: &Waker,
        error: <NA::Actor as Actor>::Error,
    ) -> Poll<()> {
        let this = self.get_unchecked_mut();
        match this.supervisor.on_error(error) {
            ActorFate::Restart(arg) => Pin::new_unchecked(this).restart_actor(waker, arg),
            ActorFate::Stop => Poll::Ready(()),
        }
    }

    unsafe fn handle_panic(
        self: Pin<&mut Self>,
        waker: &Waker,
        panic: Box<dyn Any + Send + 'static>,
    ) -> Poll<()> {
        let this = self.get_unchecked_mut();
        match this.supervisor.on_panic(panic) {
            ActorFate::Restart(arg) => Pin::new_unchecked(this).restart_actor(waker, arg),
            ActorFate::Stop => Poll::Ready(()),
        }
    }

    #[inline]
    fn run_actor(&mut self) -> <NA::Actor as Actor>::Fut<'_> {
        self.actor.run(&mut self.context)
    }

    pub async fn run(mut self) {
        let this = pin!(self);
        ActorFuture::new(this).await
    }
}

struct ActorFuture<'a, S, NA: NewActor> {
    runner: NonNull<ActorRunner<S, NA>>,
    fut: Option<<NA::Actor as Actor>::Fut<'a>>,

    _pin: PhantomPinned,
}

impl<'a, S: Supervisor<NA>, NA: NewActor> ActorFuture<'a, S, NA> {
    #[inline]
    fn new(runner: Pin<&'a mut ActorRunner<S, NA>>) -> Self {
        // SAFETY: Since pinning is not structural for the inner fields
        // of a pinned struct, it is perfectly fine to call `Actor::run`
        // through our pin-projected reference.
        //
        // Storing a pointer to `this` works for us since the original
        // pinned self ensures location stability in memory.
        //
        // Additionally, the lifetime 'a that is inferred from the Pin
        // guarantees that the struct cannot outlive the runner object
        // it references.
        let runner = unsafe { runner.get_unchecked_mut() };
        Self {
            runner: unsafe { NonNull::new_unchecked(runner as *mut _) },
            fut: Some(runner.run_actor()),

            _pin: PhantomPinned,
        }
    }
}

impl<'a, S: Supervisor<NA>, NA: NewActor> Future for ActorFuture<'a, S, NA> {
    type Output = ();

    fn poll(self: Pin<&mut Self>, cx: &mut task::Context<'_>) -> Poll<Self::Output> {
        enum Anomaly<A: Actor> {
            Failed(A::Error),
            Crashed(Box<dyn Any + Send + 'static>),
        }

        unsafe {
            let this = self.get_unchecked_mut();

            // Create a new scope so that outstanding borrows of data owned
            // by ActorRunner get dropped. Since we need to borrow the runner
            // further below for handling anomalies, this is crucial as to not
            // violate Rust's aliasing rules.
            let anomaly: Anomaly<NA::Actor> = {
                // Pin-project the future we want to poll and create it, if needed.
                // SAFETY: We're never moving the future object itself after pinning.
                let fut = Pin::new_unchecked(
                    this.fut
                        .get_or_insert_with(|| this.runner.as_mut().run_actor()),
                );

                // Poll the actor future to make some progress.
                match catch_unwind(AssertUnwindSafe(|| fut.poll(cx))) {
                    // The future is done, we need none of the data anymore
                    // and won't have to re-poll again.
                    Ok(Poll::Ready(Ok(()))) => return Poll::Ready(()),
                    // We encountered an error during execution. We need to
                    // borrow the runner to handle it, so we let the scope
                    // drop the mutable borrows of it.
                    Ok(Poll::Ready(Err(error))) => Anomaly::Failed(error),
                    // The future is not yet done.
                    Ok(Poll::Pending) => return Poll::Pending,
                    // The future panicked, which makes us unable to safely
                    // re-poll it. In that case, we also want it dropped for
                    // handling by the supervisor.
                    Err(panic) => Anomaly::Crashed(panic),
                }
            };

            // When we're reaching this point, we have an anomaly to handle
            // via the supervisor and no more active borrows of the runner
            // state. So we first pin-project it and then call the supervisor.
            let runner = Pin::new_unchecked(this.runner.as_mut());
            match anomaly {
                Anomaly::Failed(error) => runner.handle_stop(cx.waker(), error),
                Anomaly::Crashed(panic) => runner.handle_panic(cx.waker(), panic),
            }
        }
    }
}

// SAFETY: ActorFuture is self-referential with a pointer to an
// ActorRunner and a future object that mutably borrows ActorRunner
// state. Since an exclusive ActorRunner reference is needed for
// creation, it is up to our implementation to avoid unsoundness.
unsafe impl<'a, S: Send, NA: NewActor + Send> Send for ActorFuture<'a, S, NA> {}
