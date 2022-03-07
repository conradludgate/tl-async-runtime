#![forbid(unsafe_code)]

#[macro_use]
extern crate educe;

use executor::{context, Executor};
use futures::channel::oneshot;
use futures::{pin_mut, FutureExt};
use pin_project::pin_project;
use std::future::Future;
use std::ops::ControlFlow;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll, Wake, Waker};

mod executor;
mod reactor;
mod ready;

/// Networking specific handlers
pub mod net;
pub use reactor::timers::Sleep;

#[pin_project]
pub struct SpawnHandle<R>(#[pin] oneshot::Receiver<R>);

impl<R> Future for SpawnHandle<R> {
    type Output = R;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let mut this = self.project();
        // poll the inner channel for the spawned future's result
        this.0.as_mut().poll(cx).map(|x| x.unwrap())
    }
}

impl Executor {
    /// Spawns a task in this executor
    pub(crate) fn spawn<F>(&self, fut: F) -> SpawnHandle<F::Output>
    where
        F: Future + Send + Sync + 'static,
        F::Output: Send + Sync + 'static,
    {
        let (sender, receiver) = oneshot::channel();

        // Pin the future. Also wrap it s.t. it sends it's output over the channel
        let fut = Box::pin(fut.map(|out| sender.send(out).unwrap_or_default()));
        // insert the task into the runtime and signal that it is ready for processing
        self.ready.signal(fut);

        // return the handle to the reciever so that it can be `await`ed with it's output value
        SpawnHandle(receiver)
    }

    /// Run a future to completion.
    ///
    /// Starts a new runtime and spawns the future on it.
    fn block_on<F, R>(self: &Arc<Executor>, fut: F) -> R
    where
        F: Future<Output = R> + Send + Sync + 'static,
        R: Send + Sync + 'static,
    {
        // register this thread as a worker
        self.register();

        // spawn a bunch of worker threads
        for i in 1..self.ready.max_waiting {
            let exec = self.clone();
            std::thread::Builder::new()
                .name(format!("tl-async-runtime-worker-{}", i))
                .spawn(move || {
                    // register this new thread as a worker in the runtime
                    exec.register();
                    // Run tasks until told to exit
                    while let ControlFlow::Continue(_) = exec.run_task() {}
                })
                .unwrap();
        }

        // Spawn the task in the newly created runtime
        let handle = self.spawn(fut);
        pin_mut!(handle);

        // Waker specifically for the main thread.
        // Used to wake up the main thread when the output value is ready
        let waker = Waker::from(Arc::new(ThreadWaker(self.clone())));
        let mut cx = Context::from_waker(&waker);

        // Run the future to completion.
        loop {
            // if the output value is ready, return
            if let Poll::Ready(res) = handle.as_mut().poll(&mut cx) {
                break res;
            }

            // make the main thread busy and also run some tasks
            self.run_task();
        }
    }
}

struct ThreadWaker(Arc<Executor>);

impl Wake for ThreadWaker {
    fn wake(self: Arc<Self>) {
        self.0.ready.wake_all();
    }
}

/// Spawn a future on the current runtime.
/// Returns a new future that can be later awaited for it's output.
/// Task execution begins eagerly, without needing you to await it
pub fn spawn<F, R>(fut: F) -> SpawnHandle<R>
where
    F: Future<Output = R> + Send + Sync + 'static,
    R: Send + Sync + 'static,
{
    context(|exec| exec.clone().spawn(fut))
}

/// Run a future to completion.
///
/// Starts a new runtime and spawns the future on it.
pub fn block_on<F, R>(fut: F) -> R
where
    F: Future<Output = R> + Send + Sync + 'static,
    R: Send + Sync + 'static,
{
    Arc::new(Executor::default()).block_on(fut)
}
