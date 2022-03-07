use crate::{Executor, Task};
use crossbeam_channel::TryRecvError;
use parking_lot::Mutex;
use std::{
    cell::RefCell,
    ops::ControlFlow,
    sync::{atomic::Ordering, Arc},
    task::{Context, Wake, Waker},
    thread,
};

thread_local! {
    static EXECUTOR: RefCell<Option<Arc<Executor>>> = RefCell::new(None);
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
    task: Arc<Mutex<Option<Task>>>,
}

impl Wake for TaskWaker {
    fn wake(self: Arc<Self>) {
        if let Some(task) = self.task.lock().take() {
            self.executor.signal_ready(task);
        }
    }
}

impl Executor {
    fn park_thread(&self) {
        // before getting parked, ensure any
        // waiting tasks are promoted to ready
        if self.reactor.book_keeping() == 0 {
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
    fn acquire_task(&self) -> ControlFlow<(), Option<Task>> {
        match self.ready.poll() {
            Ok(task) => ControlFlow::Continue(Some(task)),
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
        self.threads.write().push(thread::current());
        EXECUTOR.with(|exec| *exec.borrow_mut() = Some(self.clone()));
    }
}
