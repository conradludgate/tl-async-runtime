use std::{
    cmp::Reverse,
    pin::Pin,
    task::{Context, Poll, Waker},
    time::{Duration, Instant},
};

use futures::Future;
use parking_lot::{Mutex, MutexGuard};
use pin_project::pin_project;

use crate::executor::context;

type TimerQueue = PriorityQueue<Waker, Reverse<Instant>>;
#[derive(Default)]
pub(crate) struct Queue(Mutex<TimerQueue>);
impl Queue {
    pub fn insert(&self, instant: Instant, task: Waker) {
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
    type Item = Waker;
    type IntoIter = QueueIter<'a>;

    fn into_iter(self) -> Self::IntoIter {
        QueueIter(self.0.lock(), Instant::now())
    }
}

pub(crate) struct QueueIter<'a>(MutexGuard<'a, TimerQueue>, Instant);
impl<'a> Iterator for QueueIter<'a> {
    type Item = Waker;

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

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let instant = *self.project().instant;
        // if the future is not yet ready
        if instant > Instant::now() {
            context(|exec| exec.reactor.timers.insert(instant, cx.waker().clone()));
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

// eq/ord only by P
#[derive(Educe)]
#[educe(PartialEq(bound = "P: std::cmp::PartialEq"))]
#[educe(Eq(bound = "P: std::cmp::Eq"))]
#[educe(PartialOrd(bound = "P: std::cmp::PartialOrd"))]
#[educe(Ord(bound = "P: std::cmp::Ord"))]
struct PriorityQueueEntry<I, P>(
    #[educe(PartialEq(ignore))]
    #[educe(Eq(ignore))]
    #[educe(PartialOrd(ignore))]
    #[educe(Ord(ignore))]
    I,
    P,
);

type PriorityQueue<I, P> = Vec<PriorityQueueEntry<I, P>>;
