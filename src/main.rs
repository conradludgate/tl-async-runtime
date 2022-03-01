use futures::{pin_mut, FutureExt};
use rand::Rng;
use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::future::Future;
use std::marker::PhantomData;
use std::pin::Pin;
use std::sync::{Arc, Mutex, RwLock};
use std::task::{Context, Poll, Wake, Waker};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

#[derive(Hash, PartialEq, Eq, Clone, Copy, Debug)]
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

type Task = Pin<Box<dyn Future<Output = ()> + Send + Sync + 'static>>;
struct Executor {
    threads: RwLock<Vec<ThreadWrapper>>,
    tasks: Mutex<HashMap<TaskID, Task>>,
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

struct SpawnHandle<R> {
    _marker: PhantomData<R>,
}

fn spawn<F>(fut: F) -> SpawnHandle<F::Output>
where
    F: Future + Send + Sync + 'static,
{
    EXECUTOR.with(|exec| {
        let id = TaskID::new();
        let exec = exec.borrow();
        let exec = exec.as_deref().unwrap();

        exec.tasks
            .lock()
            .unwrap()
            .insert(id, Box::pin(fut.map(|_| ())));

        exec.ready.write().unwrap().push_back(id);
        for t in exec.threads.read().unwrap().iter() {
            t.thread.thread().unpark();
        }
    });

    SpawnHandle {
        _marker: PhantomData,
    }
}

/// Run a future to completion on the current thread.
fn block_on<Fut: Future>(fut: Fut) -> Fut::Output {
    let executor = Arc::new(Executor {
        threads: RwLock::new(Vec::new()),
        tasks: Mutex::new(HashMap::new()),
        timers: RwLock::new(Vec::new()),
        ready: RwLock::new(VecDeque::new()),
    });
    let waker = Waker::from(executor.clone());
    EXECUTOR.with(|exec| exec.borrow_mut().replace(executor.clone()));

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
                    let mut fut = match exec.tasks.lock().unwrap().remove(&task) {
                        Some(fut) => fut,
                        None => continue,
                    };

                    // poll the fut
                    if let Poll::Ready(res) = fut.as_mut().poll(&mut cx) {
                        // task is done. clean up
                        exec.tasks.lock().unwrap().remove(&task);
                        break res;
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

        for t in executor.threads.read().unwrap().iter() {
            t.thread.thread().unpark();
        }
    }
}

fn main() {
    block_on(async {
        for i in 0..6 {
            spawn(print_from_thread(i));
        }
        thread::sleep(Duration::from_millis(1000));
        for i in 6..12 {
            spawn(print_from_thread(i));
        }
        thread::sleep(Duration::from_millis(1000));
        for i in 12..18 {
            spawn(print_from_thread(i));
        }
        thread::sleep(Duration::from_millis(1000));
        print_from_thread(20).await;
    });
}

async fn print_from_thread(i: usize) {
    println!(
        "Hi from inside a future {}! thread={:?}",
        i,
        thread::current().id()
    );
}
