use crate::{Executor, Task, TaskId};
use crossbeam_channel::TryRecvError;
use std::{
    cell::RefCell,
    ops::ControlFlow,
    sync::{atomic::Ordering, Arc},
    task::{Context, Poll, Wake, Waker},
    thread,
};

thread_local! {
    static EXECUTOR: RefCell<Option<Arc<Executor>>> = RefCell::new(None);
    static TASK_ID: RefCell<TaskId> = RefCell::new(TaskId(0));
}

pub(crate) fn task_context<R>(f: impl FnOnce(TaskId) -> R) -> R {
    TASK_ID.with(|id| f(*id.borrow()))
}

pub(crate) fn executor_context<R>(f: impl FnOnce(&Arc<Executor>) -> R) -> R {
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
        self.executor.signal_ready(self.task);
    }
}

impl Executor {
    fn park_thread(&self) {
        // before getting parked, ensure any
        // waiting tasks are promoted to ready
        if self.book_keeping() == 0 {
            // Skip if parking would cause all threads to be parked.
            // We need at least 1 thread running the books.
            let parked = self.parked.fetch_add(1, Ordering::Relaxed);
            if parked + 1 < self.threads.read().len() {
                thread::park();
            }
            self.parked.fetch_sub(1, Ordering::Release);
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
        // remove a task from the ledger to work on
        let (task, mut fut) = match self.acquire_task()? {
            Some(task) => task,
            None => return ControlFlow::Continue(()),
        };

        // register the task id
        TASK_ID.with(|id| *id.borrow_mut() = task);

        // Create a new waker for the current task.
        // When the wake is called, it tells the executor
        // that the task is once again ready for work
        // and will be picked up by an available thread
        let executor = self.clone();
        let waker = Waker::from(Arc::new(TaskWaker { task, executor }));
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
        self.threads.write().push(thread::current());
        EXECUTOR.with(|exec| *exec.borrow_mut() = Some(self.clone()));
    }
}
