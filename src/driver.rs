use std::{
    cell::RefCell,
    ops::ControlFlow,
    sync::Arc,
    task::{Context, Poll, Wake, Waker},
    thread,
};

use crossbeam_channel::TryRecvError;

use crate::{Executor, Task, TaskId};

thread_local! {
    static EXECUTOR: RefCell<Option<Arc<Executor>>> = RefCell::new(None);
    static TASK_ID: RefCell<TaskId> = RefCell::new(TaskId(0));
}

pub(crate) fn task_context<F, R>(f: F) -> R
where
    F: FnOnce(TaskId) -> R,
{
    TASK_ID.with(|id| f(*id.borrow()))
}

pub(crate) fn executor_context<F, R>(f: F) -> R
where
    F: FnOnce(&Arc<Executor>) -> R,
{
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
    task: TaskId,
}

impl Wake for TaskWaker {
    fn wake(self: Arc<Self>) {
        self.wake_by_ref();
    }
    fn wake_by_ref(self: &Arc<Self>) {
        self.executor.signal_ready(self.task);
    }
}

impl Executor {
    fn park_thread(&self) {
        // Skip if parking would cause all threads to be parked.
        // We need at least 1 thread running the books.
        if Arc::strong_count(&self.parked) + 1 < self.threads.read().unwrap().len() {
            let _park = self.parked.clone(); // increment the counter. decrements automatically
            thread::park();
        }
    }

    /// Get a single task from the queue.
    ///
    /// Parks if there are no tasks available.
    /// Returns Break if the task queue is broken.
    fn acquire_task(&self) -> ControlFlow<(), Option<(TaskId, Task)>> {
        match self.ready.poll() {
            Ok(task) => match self.tasks.remove(&task) {
                Some(fut) => ControlFlow::Continue(Some((task, fut))),
                None => ControlFlow::Continue(None),
            },
            Err(TryRecvError::Empty) => {
                // if no tasks are available, park the thread.
                // threads are woken up randomly when new tasks become available
                self.park_thread();
                ControlFlow::Continue(None)
            }
            Err(TryRecvError::Disconnected) => {
                // queue has closed, this means the block_on main thread has exited
                ControlFlow::Break(())
            }
        }
    }

    /// Try run a single task.
    ///
    /// Parks if there are no tasks available.
    /// Returns Break if the task queue is broken.
    pub(crate) fn run_task(self: &Arc<Self>) -> ControlFlow<()> {
        // before doing any work and potentially getting parked, ensure any
        // waiting tasks are promoted to ready
        self.book_keeping();

        // remove a task from the ledger to work on
        let (task, mut fut) = match self.acquire_task()? {
            Some(task) => task,
            None => return ControlFlow::Continue(()),
        };

        // register the task id
        TASK_ID.with(|id| *id.borrow_mut() = task);

        let waker = Waker::from(Arc::new(TaskWaker {
            task,
            executor: self.clone(),
        }));
        let mut cx = Context::from_waker(&waker);

        match fut.as_mut().poll(&mut cx) {
            Poll::Ready(()) => {}
            Poll::Pending => {
                // if the task isn't yet ready, put it back on the task list.
                // It's the task's responsibility to wake it self up (eg timer queue or IO events)
                self.tasks.insert(task, fut);
            }
        }
        ControlFlow::Continue(())
    }

    /// register this executor on the current thread
    pub(crate) fn register(self: &Arc<Self>) {
        self.threads.write().unwrap().push(thread::current());
        EXECUTOR.with(|exec| *exec.borrow_mut() = Some(self.clone()));
    }
}
