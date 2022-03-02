use crossbeam_channel::{bounded, unbounded, Receiver, Sender, TryRecvError};
use futures::FutureExt;
use pin_project::pin_project;
use rand::Rng;
use std::cell::RefCell;
use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap};
use std::future::Future;
use std::marker::PhantomData;
use std::ops::Add;
use std::pin::Pin;
use std::sync::{Arc, Mutex, RwLock};
use std::task::{Context, Poll, Wake, Waker};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};
#[derive(Hash, PartialEq, Eq, Clone, Copy, Debug, PartialOrd, Ord)]
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
    timers: RwLock<BinaryHeap<(Reverse<Instant>, TaskID)>>,
    ready_reciever: Receiver<TaskID>,
    ready_sender: Sender<TaskID>,
}

impl Executor {
    fn get_task(self: &Arc<Self>) -> Result<TaskID, TryRecvError> {
        // println!("getting task {:?}", thread::current().id());
        self.ready_reciever.try_recv()
    }

    fn spawn<F>(self: &Arc<Self>, fut: F) -> SpawnHandle<F::Output>
    where
        F: Future + Send + Sync + 'static,
    {
        let id = TaskID::new();

        // println!("spawn: acquiring lock on tasks");
        self.tasks
            .lock()
            .unwrap()
            .insert(id, Box::pin(fut.map(|_| ())));
        // println!("spawn: released lock on tasks");

        self.ready_sender.send(id).unwrap();

        let mut threads = self.threads.write().unwrap();
        threads.rotate_left(1);
        threads[0].thread.thread().unpark();

        SpawnHandle {
            _marker: PhantomData,
        }
    }
}

impl Wake for Executor {
    fn wake(self: Arc<Self>) {}
    fn wake_by_ref(self: &Arc<Self>) {}
}

thread_local! {
    static EXECUTOR: RefCell<Option<Arc<Executor>>> = RefCell::new(None);
    static TASK_ID: RefCell<TaskID> = RefCell::new(TaskID(0));
}

pub struct SpawnHandle<R> {
    _marker: PhantomData<R>,
}

pub fn spawn<F>(fut: F) -> SpawnHandle<F::Output>
where
    F: Future + Send + Sync + 'static,
{
    EXECUTOR.with(|exec| {
        let exec = exec.borrow();
        let exec = exec.as_ref().unwrap();

        exec.spawn(fut)
    })
}

#[pin_project]
pub struct Sleep {
    instant: Instant,
}

impl Future for Sleep {
    type Output = ();

    fn poll(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Self::Output> {
        let instant = *self.project().instant;
        if instant > Instant::now() {
            TASK_ID.with(|id| {
                let id = *id.borrow();
                EXECUTOR.with(|exec| {
                    let exec = exec.borrow();
                    exec.as_deref()
                        .unwrap()
                        .timers
                        .write()
                        .unwrap()
                        .push((Reverse(instant), id));
                    Poll::Pending
                })
            })
        } else {
            Poll::Ready(())
        }
    }
}

impl Sleep {
    pub async fn until(instant: Instant) {
        Self { instant }.await
    }
    pub async fn duration(duration: Duration) {
        Sleep::until(Instant::now().add(duration)).await
    }
}

/// Run a future to completion on the current thread.
pub fn block_on<Fut: Future>(fut: Fut) -> Fut::Output
where
    Fut::Output: Send + Sync + 'static,
    Fut: Send + Sync + 'static,
{
    let (ready_sender, ready_reciever) = unbounded::<TaskID>();
    let executor = Arc::new(Executor {
        threads: RwLock::new(Vec::new()),
        tasks: Mutex::new(HashMap::new()),
        timers: RwLock::new(BinaryHeap::new()),
        ready_reciever,
        ready_sender,
    });

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

                    TASK_ID.with(|id| {
                        *id.borrow_mut() = task;

                        // poll the fut
                        match fut.as_mut().poll(&mut cx) {
                            Poll::Ready(()) => {}
                            Poll::Pending => {
                                // println!("thread: acquiring lock on tasks 2");
                                exec.tasks.lock().unwrap().insert(task, fut);
                                // println!("thread: releasing lock on tasks 2");
                            }
                        }
                    });
                }
            })
            .unwrap();

        executor
            .threads
            .write()
            .unwrap()
            .push(ThreadWrapper { thread })
    }

    let (final_sender, final_reciever) = bounded::<Fut::Output>(1);
    let final_sender = Arc::new(final_sender);
    let fut = fut.map(move |out| {
        final_sender.send(out).unwrap();
    });

    executor.spawn(fut);

    // Run the future to completion.
    loop {
        if let Ok(res) = final_reciever.try_recv() {
            break res;
        }
        // get the current task timers that have elapsed and insert them into the ready tasks
        let instant = Instant::now();
        let mut timers = executor.timers.write().unwrap();
        while timers.peek().map(|(t, _)| t.0 < instant).unwrap_or(false) {
            let (_, id) = timers.pop().unwrap();
            executor.ready_sender.send(id).unwrap();

            let mut threads = executor.threads.write().unwrap();
            threads.rotate_left(1);
            threads[0].thread.thread().unpark();
        }
    }
}
