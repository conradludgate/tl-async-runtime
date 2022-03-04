use std::{
    cmp::Reverse,
    collections::BinaryHeap,
    pin::Pin,
    task::{Context, Poll},
    time::{Duration, Instant},
};

use futures::Future;
use parking_lot::{Mutex, MutexGuard};

use crate::{
    driver::{executor_context, task_context},
    TaskId,
};

type TimerHeap = BinaryHeap<(Reverse<Instant>, TaskId)>;
#[derive(Default)]
pub(crate) struct Queue(Mutex<TimerHeap>);
impl Queue {
    pub fn insert(&self, instant: Instant, task: TaskId) {
        self.0.lock().push((Reverse(instant), task));
    }
}

impl<'a> IntoIterator for &'a Queue {
    type Item = TaskId;
    type IntoIter = QueueIter<'a>;

    fn into_iter(self) -> Self::IntoIter {
        QueueIter(self.0.lock(), Instant::now())
    }
}

pub(crate) struct QueueIter<'a>(MutexGuard<'a, TimerHeap>, Instant);
impl<'a> Iterator for QueueIter<'a> {
    type Item = TaskId;

    fn next(&mut self) -> Option<Self::Item> {
        let (Reverse(time), task) = self.0.pop()?;
        if time > self.1 {
            self.0.push((Reverse(time), task));
            None
        } else {
            Some(task)
        }
    }
}

/// Future for sleeping fixed amounts of time.
/// Does not block the thread
pub struct Sleep {
    instant: Instant,
}

impl Future for Sleep {
    type Output = ();

    fn poll(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Self::Output> {
        // if the future is not yet ready
        if self.instant > Instant::now() {
            task_context(|id| {
                executor_context(|exec| {
                    // register the timer on the executor
                    exec.timers.insert(self.instant, id);
                })
            });
            Poll::Pending
        } else {
            Poll::Ready(())
        }
    }
}

impl Sleep {
    /// sleep until a specific point in time
    pub fn until(instant: Instant) -> Sleep {
        Self { instant }
    }
    /// sleep for a specific duration of time
    pub fn duration(duration: Duration) -> Sleep {
        Sleep::until(Instant::now() + duration)
    }
}
