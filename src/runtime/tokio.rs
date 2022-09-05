//! [`tokio`] runtime support for monroe.

use std::{future::Future, io};

use tokio::{runtime as tokio_rt, task::JoinSet};
use usync::{const_mutex, Mutex};

use super::Handle;

#[inline]
fn make_default_tokio_runtime() -> io::Result<tokio_rt::Runtime> {
    tokio_rt::Builder::new_multi_thread()
        .enable_all()
        .thread_name("monroe-tokio-runtime-worker")
        .build()
}

/// An asynchronous, multi-threaded actor runtime based on
/// [tokio].
///
/// It will have all available tokio runtime features enabled
/// by default (networking and timers at the time of writing).
pub struct Runtime {
    rt: tokio_rt::Runtime,
    actors: Mutex<JoinSet<()>>,
}

impl Runtime {
    /// Creates a new runtime with configuration as stated in
    /// the documentation of [`Runtime`].
    pub fn new() -> io::Result<Self> {
        make_default_tokio_runtime().map(|rt| Self {
            rt,
            actors: const_mutex(JoinSet::new()),
        })
    }

    /// Gets an immutable reference to the "original" [`tokio`]
    /// runtime.
    pub fn original(&self) -> &tokio_rt::Runtime {
        &self.rt
    }
}

impl super::Runtime for Runtime {
    fn block_on<Fn, F>(self, f: Fn) -> F::Output
    where
        Fn: FnOnce(Handle<Self>) -> F,
        F: Future,
    {
        let handle = Handle::new(self);
        handle.original().block_on(f(handle.clone()))
    }

    fn stop(&self) {
        let mut actors = self.actors.lock();
        actors.abort_all();
    }

    fn spawn<F>(&self, fut: F)
    where
        F: std::future::Future + Send + 'static,
        F::Output: Send + 'static,
    {
        let mut actors = self.actors.lock();
        actors.spawn(async move {
            let _ = fut.await;
        });
    }
}
