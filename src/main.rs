use crossbeam_channel::{unbounded, Receiver, Sender, TryRecvError};
use futures::{pin_mut, FutureExt};
use rand::Rng;
use std::cell::RefCell;
use std::collections::HashMap;
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
    // parked: AtomicBool,
}
type Task = Pin<Box<dyn Future<Output = ()> + Send + Sync + 'static>>;
struct Executor {
    threads: RwLock<Vec<ThreadWrapper>>,
    tasks: Mutex<HashMap<TaskID, Task>>,
    timers: RwLock<Vec<(TaskID, Instant)>>,
    ready_reciever: Receiver<TaskID>,
    ready_sender: Sender<TaskID>,
}

impl Executor {
    fn get_task(self: &Arc<Self>) -> Result<TaskID, TryRecvError> {
        // println!("getting task {:?}", thread::current().id());
        self.ready_reciever.try_recv()
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

        // println!("spawn: acquiring lock on tasks");
        exec.tasks
            .lock()
            .unwrap()
            .insert(id, Box::pin(fut.map(|_| ())));
        // println!("spawn: released lock on tasks");

        exec.ready_sender.send(id).unwrap();

        for t in exec.threads.read().unwrap().iter() {
            // println!("telling {:?} to unpark", t.thread.thread().id());
            t.thread.thread().unpark();
        }
    });

    SpawnHandle {
        _marker: PhantomData,
    }
}

/// Run a future to completion on the current thread.
fn block_on<Fut: Future>(fut: Fut) -> Fut::Output {
    let (ready_sender, ready_reciever) = unbounded::<TaskID>();
    let executor = Arc::new(Executor {
        threads: RwLock::new(Vec::new()),
        tasks: Mutex::new(HashMap::new()),
        timers: RwLock::new(Vec::new()),
        ready_reciever,
        ready_sender,
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
                            Ok(task) => break task,
                            Err(TryRecvError::Empty) => {
                                // println!("parking {:?}", thread::current().id());
                                thread::park();
                                // println!("resuming {:?}", thread::current().id());
                            }
                            Err(TryRecvError::Disconnected) => {
                                return;
                            }
                        }
                    };

                    // Get the fut for the task
                    // println!("thread: acquiring lock on tasks 1");
                    let fut = exec.tasks.lock().unwrap().remove(&task);
                    // println!("thread: releasing lock on tasks 1");
                    let mut fut = match fut {
                        Some(fut) => fut,
                        None => continue,
                    };

                    // poll the fut
                    match fut.as_mut().poll(&mut cx) {
                        Poll::Ready(()) => {}
                        Poll::Pending => {
                            // println!("thread: acquiring lock on tasks 2");
                            exec.tasks.lock().unwrap().insert(task, fut);
                            // println!("thread: releasing lock on tasks 2");
                        }
                    }
                }
            })
            .unwrap();

        executor
            .threads
            .write()
            .unwrap()
            .push(ThreadWrapper { thread })
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
        for task in tasks.drain(..=index) {
            executor.ready_sender.send(task.0).unwrap()
        }

        for t in executor.threads.read().unwrap().iter() {
            t.thread.thread().unpark();
        }
    }
}

fn main() {
    block_on(async {
        println!("begin dispatch");
        for i in 0..6 {
            spawn(print_from_thread(i));
        }
        thread::sleep(Duration::from_millis(1000));
        println!("begin dispatch");
        for i in 6..12 {
            spawn(print_from_thread(i));
        }
        thread::sleep(Duration::from_millis(1000));
        println!("begin dispatch");
        for i in 12..18 {
            spawn(print_from_thread(i));
        }
        thread::sleep(Duration::from_millis(1000));
        println!("begin dispatch");
        print_from_thread(20).await;
    });
}

async fn print_from_thread(i: usize) {
    thread::sleep(Duration::from_millis(10));
    println!("Hi from inside future {}! {:?}", i, thread::current().id());
}
