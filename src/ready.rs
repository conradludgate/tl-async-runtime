use parking_lot::{Condvar, Mutex};
use std::{
    collections::VecDeque,
    sync::atomic::{AtomicUsize, Ordering},
};

#[derive(Educe)]
#[educe(Default)]
pub(crate) struct Queue {
    len: AtomicUsize,
    tasks: Mutex<VecDeque<crate::executor::Task>>,
    waker: Condvar,
    waiting: AtomicUsize,
    #[educe(Default(expression = "get_thread_count()"))]
    pub max_waiting: usize,
}

fn get_thread_count() -> usize {
    std::thread::available_parallelism().map_or(4, |t| t.get())
}

impl Queue {
    /// Signals that a task is now ready to be worked on
    pub(crate) fn signal(&self, task: crate::executor::Task) {
        self.tasks.lock().push_back(task);
        self.len.fetch_add(1, Ordering::Relaxed);
        self.waker.notify_one();
    }

    pub fn should_wait(&self) -> bool {
        self.len.load(Ordering::SeqCst) == 0
    }

    /// Get a single task from the queue.
    ///
    /// Parks if there are no tasks available.
    /// Returns Break if the task queue is broken.
    pub fn acquire_task(&self, should_wait: bool) -> Option<crate::executor::Task> {
        let mut tasks = self.tasks.lock();
        if should_wait {
            let waiting = self.waiting.fetch_add(1, Ordering::Relaxed);
            if waiting + 1 < self.max_waiting {
                self.waker.wait(&mut tasks);
            }
            self.waiting.fetch_sub(1, Ordering::Relaxed);
        }
        match tasks.pop_front() {
            Some(t) => {
                self.len.fetch_sub(1, Ordering::Relaxed);
                Some(t)
            }
            None => None,
        }
    }
}
