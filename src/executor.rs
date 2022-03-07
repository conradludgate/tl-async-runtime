use futures::Future;
use parking_lot::Mutex;
use std::{
    cell::RefCell,
    ops::ControlFlow,
    pin::Pin,
    sync::Arc,
    task::{Context, Wake, Waker},
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
}
