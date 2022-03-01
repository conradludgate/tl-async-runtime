use futures::pin_mut;
use rand::Rng;
use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, RwLock};
use std::task::{Context, Poll, Wake, Waker};
use std::thread::{self, JoinHandle};
use std::time::Instant;

#[derive(Hash, PartialEq, Eq)]
struct TaskID(u64);

impl TaskID {
    fn new() -> Self {
        Self(rand::thread_rng().gen())
    }
}

struct ThreadWrapper {
    thread: JoinHandle<()>,
    parked: bool,
}

struct Executor {
    threads: RwLock<Vec<ThreadWrapper>>,
    tasks: RwLock<HashMap<TaskID, Pin<Box<dyn Future<Output = ()> + Send + Sync + 'static>>>>,
    timers: RwLock<Vec<(TaskID, Instant)>>,
    ready: RwLock<VecDeque<TaskID>>,
}

impl Executor {
    fn get_task(self: &Arc<Self>) -> Option<TaskID> {
        self.ready.write().unwrap().pop_front()
    }
}

impl Wake for Executor {
    fn wake(self: Arc<Self>) {}
    fn wake_by_ref(self: &Arc<Self>) {}
}

thread_local! {
    static EXECUTOR: RefCell<Option<Arc<Executor>>> = RefCell::new(None);
}

fn spawn() {}

/// Run a future to completion on the current thread.
fn block_on<Fut: Future>(fut: Fut) -> Fut::Output {
    let executor = Arc::new(Executor {
        threads: RwLock::new(Vec::new()),
        tasks: RwLock::new(HashMap::new()),
        timers: RwLock::new(Vec::new()),
        ready: RwLock::new(VecDeque::new()),
    });
    let waker = Waker::from(executor.clone());

    for _ in 0..7 {
        let exec = executor.clone();
        let thread = thread::Builder::new()
            .spawn(move || {
                let exec2 = exec.clone();
                EXECUTOR.with(|exec| exec.borrow_mut().replace(exec2));

                let waker = Waker::from(exec.clone());
                let mut cx = Context::from_waker(&waker);
                loop {
                    // get the next available task if there is one
                    let task = loop {
                        match exec.get_task() {
                            Some(task) => break task,
                            None => thread::park(),
                        }
                    };

                    // Get the fut for the task
                    let mut fut = match exec.tasks.write().unwrap().remove(&task) {
                        Some(fut) => fut,
                        None => continue,
                    };

                    // poll the fut
                    match fut.as_mut().poll(&mut cx) {
                        Poll::Ready(res) => {
                            // task is done. clean up
                            exec.tasks.write().unwrap().remove(&task);
                            break res;
                        }
                        Poll::Pending => thread::park(),
                    }
                }
            })
            .unwrap();

        executor.threads.write().unwrap().push(ThreadWrapper {
            thread,
            parked: false,
        })
    }

    // Pin the future so it can be polled.
    pin_mut!(fut);
    let mut cx = Context::from_waker(&waker);

    // Run the future to completion.
    loop {
        if let Poll::Ready(res) = fut.as_mut().poll(&mut cx) {
            break res;
        }
        // get the current task timers that have elapsed and insert them into the ready tasks
        let instant = Instant::now();
        let index = executor
            .timers
            .read()
            .unwrap()
            .binary_search_by_key(&instant, |(_, t)| *t);
        let index = match index {
            Ok(i) => i,
            Err(i) => i,
        };
        let mut tasks = executor.timers.write().unwrap();
        let tasks = tasks.drain(..=index);
        executor
            .ready
            .write()
            .unwrap()
            .extend(tasks.map(|(t, _)| t));
    }
}

fn main() {
    block_on(async {
        pause().await;
        println!("Hi from inside a future!");
    });
}

async fn pause() {}
