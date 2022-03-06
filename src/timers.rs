use std::{
    pin::Pin,
    task::{Context, Poll},
    time::{Duration, Instant}, cmp::Reverse,
};

use futures::Future;
use parking_lot::{Mutex, MutexGuard};
use pin_project::pin_project;

use crate::{
    driver::{executor_context, task_context},
    TaskId,
};

type TimerQueue = PriorityQueue<TaskId, Reverse<Instant>>;
#[derive(Default)]
pub(crate) struct Queue(Mutex<TimerQueue>);
impl Queue {
    pub fn insert(&self, instant: Instant, task: TaskId) {
        let entry = PriorityQueueEntry(task, Reverse(instant));
        let mut queue = self.0.lock();
        let index = match queue.binary_search(&entry) {
            Ok(index) => index,
            Err(index) => index,
        };
        queue.insert(index, entry);
    }
}

impl<'a> IntoIterator for &'a Queue {
    type Item = TaskId;
    type IntoIter = QueueIter<'a>;

    fn into_iter(self) -> Self::IntoIter {
        QueueIter(self.0.lock(), Instant::now())
    }
}

pub(crate) struct QueueIter<'a>(MutexGuard<'a, TimerQueue>, Instant);
impl<'a> Iterator for QueueIter<'a> {
    type Item = TaskId;

    fn next(&mut self) -> Option<Self::Item> {
        let PriorityQueueEntry(task, Reverse(time)) = self.0.pop()?;
        if time > self.1 {
            self.0.push(PriorityQueueEntry(task, Reverse(time)));
            None
        } else {
            Some(task)
        }
    }
}

/// Future for sleeping fixed amounts of time.
/// Does not block the thread
#[pin_project]
pub struct Sleep {
    instant: Instant,
}

impl Future for Sleep {
    type Output = ();

    fn poll(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Self::Output> {
        let instant = *self.project().instant;
        // if the future is not yet ready
        if instant > Instant::now() {
            task_context(|id| {
                executor_context(|exec| {
                    // register the timer on the executor
                    exec.timers.insert(instant, id);
                    Poll::Pending
                })
            })
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

struct PriorityQueueEntry<I, P>(I, P);
impl<I, P: PartialEq> PartialEq for PriorityQueueEntry<I, P> {
    fn eq(&self, other: &Self) -> bool {
        self.1 == other.1
    }
}
impl<I, P: Eq> Eq for PriorityQueueEntry<I, P> {}
impl<I, P: PartialOrd> PartialOrd for PriorityQueueEntry<I, P> {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        self.1.partial_cmp(&other.1)
    }
}

impl<I, P: Ord> Ord for PriorityQueueEntry<I, P> {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.1.cmp(&other.1)
    }
}

type PriorityQueue<I, P> = Vec<PriorityQueueEntry<I, P>>;
