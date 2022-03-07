use futures::{channel::oneshot, pin_mut, Future, FutureExt};
use parking_lot::Mutex;
use pin_project::pin_project;
use std::{
    cell::RefCell,
    ops::ControlFlow,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll, Wake, Waker},
};

use crate::{reactor::Reactor, ready};

thread_local! {
    static EXECUTOR: RefCell<Option<Arc<Executor>>> = RefCell::new(None);
}

pub(crate) fn context<R>(f: impl FnOnce(&Arc<Executor>) -> R) -> R {
    EXECUTOR.with(|exec| {
        let exec = exec.borrow();
        let exec = exec
            .as_ref()
            .expect("spawn called outside of an executor context");
        f(exec)
    })
}

struct TaskWaker {
    executor: Arc<Executor>,
    task: Arc<Mutex<Option<Task>>>,
}

impl Wake for TaskWaker {
    fn wake(self: Arc<Self>) {
        if let Some(task) = self.task.lock().take() {
            self.executor.ready.signal(task);
        }
    }
}

pub type Task = Pin<Box<dyn Future<Output = ()> + Send + Sync + 'static>>;
#[derive(Default)]
pub struct Executor {
    pub(crate) reactor: Reactor,
    pub(crate) ready: ready::Queue,
}

impl Executor {
    /// Get a single task from the queue.
    ///
    /// Parks if there are no tasks available.
    fn acquire_task(&self) -> ControlFlow<(), Option<Task>> {
        let should_wait = self.ready.should_wait() && self.reactor.book_keeping() == 0;
        ControlFlow::Continue(self.ready.acquire_task(should_wait))
    }

    /// Try run a single task.
    ///
    /// Parks if there are no tasks available.
    /// Returns Break if the task queue is broken.
    pub(crate) fn run_task(self: &Arc<Self>) -> ControlFlow<()> {
        // remove a task from the ledger to work on
        let mut fut = match self.acquire_task()? {
            Some(fut) => fut,
            None => return ControlFlow::Continue(()),
        };

        let task_ref = Arc::new(Mutex::new(None));

        // Create a new waker for the current task.
        // When the wake is called, it tells the executor
        // that the task is once again ready for work
        // and will be picked up by an available thread
        let executor = self.clone();
        let waker = Waker::from(Arc::new(TaskWaker {
            task: task_ref.clone(),
            executor,
        }));
        let mut cx = Context::from_waker(&waker);

        if fut.as_mut().poll(&mut cx).is_pending() {
            task_ref.lock().replace(fut);
        }
        ControlFlow::Continue(())
    }

    /// register this executor on the current thread
    pub(crate) fn register(self: &Arc<Self>) {
        EXECUTOR.with(|exec| *exec.borrow_mut() = Some(self.clone()));
    }

    /// Spawns a task in this executor
    pub(crate) fn spawn<F>(&self, fut: F) -> JoinHandle<F::Output>
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
        JoinHandle(receiver)
    }

    /// Run a future to completion.
    ///
    /// Starts a new runtime and spawns the future on it.
    pub fn block_on<F, R>(self: &Arc<Executor>, fut: F) -> R
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

#[pin_project]
pub struct JoinHandle<R>(#[pin] oneshot::Receiver<R>);

impl<R> Future for JoinHandle<R> {
    type Output = R;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let mut this = self.project();
        // poll the inner channel for the spawned future's result
        this.0.as_mut().poll(cx).map(|x| x.unwrap())
    }
}

struct ThreadWaker(Arc<Executor>);

impl Wake for ThreadWaker {
    fn wake(self: Arc<Self>) {
        self.0.ready.wake_all();
    }
}
