#![forbid(unsafe_code)]
#![feature(pin_macro, poll_ready, bool_to_option)]

use chashmap::CHashMap;
use driver::executor_context;
use futures::channel::oneshot;
use futures::FutureExt;
use parking_lot::{Mutex, RwLock};
use pin_project::pin_project;
use rand::Rng;
use std::future::Future;
use std::ops::ControlFlow;
use std::pin::{pin, Pin};
use std::sync::Arc;
use std::task::{Context, Poll, Wake, Waker};
use std::thread::{self, Thread};

mod driver;
mod ready;

/// Tools used for communicating with the OS
pub mod io;
/// Timers used for pausing tasks for fixed durations
pub mod timers;

#[derive(Hash, PartialEq, Eq, Clone, Copy, Debug, PartialOrd, Ord)]
struct TaskId(usize);

type Task = Pin<Box<dyn Future<Output = ()> + Send + Sync + 'static>>;
#[derive(Default)]
struct Executor {
    threads: RwLock<Vec<Thread>>,
    tasks: CHashMap<TaskId, Task>,
    timers: timers::Queue,
    ready: ready::Queue,
    os: Mutex<io::Os>,
    parked: Arc<()>,
}

#[pin_project]
pub struct SpawnHandle<R>(#[pin] oneshot::Receiver<R>);

impl<R> Future for SpawnHandle<R> {
    type Output = R;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        // poll the inner channel for the spawned future's result
        self.project().0.as_mut().poll(cx).map(|x| x.unwrap())
    }
}

impl Executor {
    /// Signals that a task is now ready to be worked on
    pub(crate) fn signal_ready(&self, id: TaskId) {
        self.ready.push(id);

        // get a 'random' thread from the loop and wake it up.
        // does nothing if the thread is not parked.
        let mut threads = self.threads.write();
        threads.rotate_left(1);
        threads[0].unpark();
    }

    /// Spawns a task in this executor
    pub(crate) fn spawn<F>(&self, fut: F) -> SpawnHandle<F::Output>
    where
        F: Future + Send + Sync + 'static,
        F::Output: Send + Sync + 'static,
    {
        // get a random task id and channel to send the results over
        let id = TaskId(rand::thread_rng().gen());
        let (sender, receiver) = oneshot::channel();

        // Pin the future. Also wrap it s.t. it sends it's output over the channel
        let fut = fut.map(|out| sender.send(out).unwrap_or(()));
        // insert the task into the runtime and signal that it is ready for processing
        self.tasks.insert(id, Box::pin(fut));
        self.signal_ready(id);

        // return the handle to the reciever so that it can be `await`ed with it's output value
        SpawnHandle(receiver)
    }

    // this is run by any thread that currently is not busy.
    // It manages the timers and OS polling in order to wake up tasks
    fn book_keeping(&self) {
        // get the current task timers that have elapsed and insert them into the ready tasks
        self.timers.into_iter().for_each(|id| self.signal_ready(id));

        // get the OS events
        let mut os = self.os.lock();
        os.process()
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
        let threads = thread::available_parallelism().map_or(4, |t| t.get());
        eprintln!("running on {threads} threads");
        for i in 1..threads {
            let exec = self.clone();
            thread::Builder::new()
                .name(format!("tl-async-runtime-worker-{}", i))
                .spawn(move || {
                    // register this new thread as a worker in the runtime
                    exec.register();
                    // Run tasks until told to exit
                    while let ControlFlow::Continue(_) = exec.run_task() {}
                })
                .unwrap();
        }

        // Waker specifically for the main thread.
        // Used to wake up the main thread when the output value is ready
        let waker = Waker::from(Arc::new(ThreadWaker(thread::current())));
        let mut cx = Context::from_waker(&waker);

        // Spawn the task in the newly created runtime
        let mut handle = pin!(self.spawn(fut));

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

struct ThreadWaker(Thread);

impl Wake for ThreadWaker {
    fn wake(self: Arc<Self>) {
        self.0.unpark();
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
    executor_context(|exec| exec.clone().spawn(fut))
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
