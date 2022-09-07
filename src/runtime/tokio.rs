//! [`tokio`] runtime support for monroe.

use std::{future::Future, io, sync::Arc};

use tokio::{runtime as tokio_rt, task::JoinSet};
use usync::{const_mutex, Mutex};

type Tasks = Arc<Mutex<JoinSet<()>>>;

#[inline]
fn make_default_tokio_runtime() -> io::Result<tokio_rt::Runtime> {
    tokio_rt::Builder::new_multi_thread()
        .enable_all()
        .thread_name("monroe-runtime-worker")
        .build()
}

/// An asynchronous, multi-threaded actor runtime based on
/// [tokio].
///
/// It will have all available tokio runtime features enabled
/// by default (networking and timers at the time of writing).
pub struct Runtime {
    rt: tokio_rt::Runtime,
    tasks: Tasks,
}

/// A handle to the tokio [`Runtime`].
///
/// All actors running under a [`Runtime`] can access this
/// type through their context to spawn more actors or
/// manipulate their own runtime behavior.
#[derive(Clone)]
pub struct RuntimeHandle(Tasks);

impl Runtime {
    /// Creates a new runtime with configuration as stated in
    /// the documentation of [`Runtime`].
    pub fn new() -> io::Result<Self> {
        make_default_tokio_runtime().map(|rt| Self {
            rt,
            tasks: Arc::new(const_mutex(JoinSet::new())),
        })
    }

    /// Gets an immutable reference to the "original" [`tokio`]
    /// runtime.
    pub fn original(&self) -> &tokio_rt::Runtime {
        &self.rt
    }
}

impl super::Runtime for Runtime {
    type Handle = RuntimeHandle;

    fn handle(&self) -> Self::Handle {
        RuntimeHandle(self.tasks.clone())
    }

    fn block_on<Fn, F>(&self, f: Fn) -> F::Output
    where
        Fn: FnOnce(Self::Handle) -> F,
        F: Future,
    {
        self.rt.block_on(f(self.handle()))
    }
}

impl super::RuntimeHandle for RuntimeHandle {
    fn stop(&self) {
        let mut tasks = self.0.lock();
        tasks.abort_all();
    }

    fn spawn<F>(&self, fut: F)
    where
        F: Future + Send + 'static,
        F::Output: Send + 'static,
    {
        let mut tasks = self.0.lock();
        tasks.spawn(async move {
            let _ = fut.await;
        });
    }
}
