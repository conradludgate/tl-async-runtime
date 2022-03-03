use chashmap::CHashMap;
use driver::executor_context;
use futures::channel::oneshot;
use futures::{pin_mut, FutureExt};
use parking_lot::{Mutex, RwLock};
use pin_project::pin_project;
use rand::Rng;
use std::future::Future;
use std::ops::ControlFlow;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll, Wake, Waker};
use std::thread::{self, Thread};

mod driver;
pub mod io;
mod ready;
pub mod timers;

#[derive(Hash, PartialEq, Eq, Clone, Copy, Debug, PartialOrd, Ord)]
struct TaskId(usize);

impl TaskId {
    fn new() -> Self {
        Self(rand::thread_rng().gen())
    }
}

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
pub struct SpawnHandle<R> {
    #[pin]
    receiver: oneshot::Receiver<R>,
}

impl<R> Future for SpawnHandle<R> {
    type Output = R;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let mut this = self.project();
        // poll the inner channel for the spawned future's result
        this.receiver.as_mut().poll(cx).map(|x| x.unwrap())
    }
}

impl Executor {
    pub(crate) fn signal_ready(&self, id: TaskId) {
        self.ready.push(id);

        // get a 'random' thread from the loop and wake it up.
        // does nothing if the thread is not parked.
        let mut threads = self.threads.write();
        threads.rotate_left(1);
        threads[0].unpark();
    }

    fn spawn<F>(self: Arc<Self>, fut: F) -> SpawnHandle<F::Output>
    where
        F: Future + Send + Sync + 'static,
        F::Output: Send + Sync + 'static,
    {
        let id = TaskId::new();
        let (sender, receiver) = oneshot::channel();

        let fut = Box::pin(fut.map(|out| sender.send(out).unwrap_or_default()));
        self.tasks.insert(id, fut);
        self.signal_ready(id); // new tasks are instantly ready

        SpawnHandle { receiver }
    }

    // this is run by any thread that currently is not busy.
    // It manages the timers and OS polling in order to wake up tasks
    fn book_keeping(&self) {
        // get the current task timers that have elapsed and insert them into the ready tasks
        for id in &self.timers {
            self.signal_ready(id);
        }

        // get the OS events
        let mut os = self.os.lock();
        os.process(self)
    }
}

struct ThreadWaker {
    thread: Thread,
}

impl Wake for ThreadWaker {
    fn wake(self: Arc<Self>) {
        self.wake_by_ref();
    }
    fn wake_by_ref(self: &Arc<Self>) {
        self.thread.unpark();
    }
}

pub fn spawn<F, R>(fut: F) -> SpawnHandle<R>
where
    F: Future<Output = R> + Send + Sync + 'static,
    R: Send + Sync + 'static,
{
    executor_context(|exec| exec.clone().spawn(fut))
}

/// Run a future to completion on the current thread.
pub fn block_on<Fut: Future>(fut: Fut) -> Fut::Output
where
    Fut::Output: Send + Sync + 'static,
    Fut: Send + Sync + 'static,
{
    let executor = Arc::new(Executor::default());
    executor.register();

    // spawn a bunch of worker threads
    for i in 1..8 {
        let exec = executor.clone();
        thread::Builder::new()
            .name(format!("tl-async-runtime-worker-{}", i))
            .spawn(move || {
                exec.register();
                while let ControlFlow::Continue(_) = exec.run_task() {}
            })
            .unwrap();
    }

    let handle = executor.clone().spawn(fut);
    pin_mut!(handle);

    let waker = Waker::from(Arc::new(ThreadWaker {
        thread: thread::current(),
    }));
    let mut cx = Context::from_waker(&waker);

    // Run the future to completion.
    loop {
        // if the output value is ready, return
        if let Poll::Ready(res) = handle.as_mut().poll(&mut cx) {
            break res;
        }
        executor.run_task();
    }
}
