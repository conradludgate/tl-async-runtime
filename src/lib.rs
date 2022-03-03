use crossbeam_channel::{unbounded, Receiver, Sender, TryRecvError};
use futures::channel::oneshot;
use futures::{pin_mut, FutureExt};
use pin_project::pin_project;
use rand::Rng;
use std::cell::RefCell;
use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap};
use std::future::Future;
use std::ops::ControlFlow;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::task::{Context, Poll, Wake, Waker};
use std::thread::{self, Thread};
use std::time::{Duration, Instant};
#[derive(Hash, PartialEq, Eq, Clone, Copy, Debug, PartialOrd, Ord)]
struct TaskID(u64);

impl TaskID {
    fn new() -> Self {
        Self(rand::thread_rng().gen())
    }
}

struct ThreadWrapper {
    thread: Thread,
    // parked: AtomicBool,
}

struct Os {
    poll: mio::Poll,
    events: mio::Events,
}

impl Default for Os {
    fn default() -> Self {
        Self {
            poll: mio::Poll::new().unwrap(),
            events: mio::Events::with_capacity(128),
        }
    }
}

struct ReadyQueue {
    receiver: Receiver<TaskID>,
    sender: Sender<TaskID>,
}

impl ReadyQueue {
    pub fn poll(&self) -> Result<TaskID, TryRecvError> {
        // println!("getting task {:?}", thread::current().id());
        self.receiver.try_recv()
    }
    pub fn push(&self, id: TaskID) {
        self.sender.send(id).unwrap();
    }
}

impl Default for ReadyQueue {
    fn default() -> Self {
        let (sender, receiver) = unbounded();
        Self { receiver, sender }
    }
}

type Task = Pin<Box<dyn Future<Output = ()> + Send + Sync + 'static>>;
#[derive(Default)]
struct Executor {
    threads: RwLock<Vec<ThreadWrapper>>,
    tasks: Mutex<HashMap<TaskID, Task>>,
    timers: Mutex<BinaryHeap<(Reverse<Instant>, TaskID)>>,
    ready: ReadyQueue,
    os: Mutex<Os>,
    parked: Arc<()>,
    done: AtomicBool,
}

pub struct OwnedTask {
    id: TaskID,
    executor: Arc<Executor>,
}
#[pin_project]
pub struct SpawnHandle<R> {
    task: OwnedTask,
    #[pin]
    receiver: oneshot::Receiver<R>,
}

impl Drop for OwnedTask {
    fn drop(&mut self) {
        self.executor.tasks.lock().unwrap().remove(&self.id);
    }
}

impl<R> Future for SpawnHandle<R> {
    type Output = R;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let mut this = self.project();
        // poll the inner channel for the spawned future's result
        this.receiver.as_mut().poll(cx).map(|x| x.unwrap())
    }
}

impl Executor {
    fn signal_ready(self: &Arc<Self>, id: TaskID) {
        self.ready.push(id);

        let mut threads = self.threads.write().unwrap();
        threads.rotate_left(1);
        threads[0].thread.unpark();
    }

    fn spawn<F>(self: &Arc<Self>, fut: F) -> SpawnHandle<F::Output>
    where
        F: Future + Send + Sync + 'static,
        F::Output: Send + Sync + 'static,
    {
        let id = TaskID::new();
        let (sender, receiver) = oneshot::channel();

        // println!("spawn: acquiring lock on tasks");
        self.tasks.lock().unwrap().insert(
            id,
            Box::pin(fut.map(|out| {
                sender.send(out).map_err(|_| ()).unwrap();
            })),
        );
        // println!("spawn: released lock on tasks");

        self.signal_ready(id);

        SpawnHandle {
            task: OwnedTask {
                id,
                executor: self.clone(),
            },
            receiver,
        }
    }
}

pub struct TaskWaker {
    executor: Arc<Executor>,
    id: TaskID,
}

impl Wake for TaskWaker {
    fn wake(self: Arc<Self>) {
        self.wake_by_ref();
    }
    fn wake_by_ref(self: &Arc<Self>) {
        self.executor.signal_ready(self.id);
    }
}

pub struct ThreadWaker {
    executor: Arc<Executor>,
    thread: Thread,
}

impl Wake for ThreadWaker {
    fn wake(self: Arc<Self>) {
        self.wake_by_ref();
    }
    fn wake_by_ref(self: &Arc<Self>) {
        self.executor.done.store(true, Ordering::SeqCst);
        self.thread.unpark();
    }
}

thread_local! {
    static EXECUTOR: RefCell<Option<Arc<Executor>>> = RefCell::new(None);
    static TASK_ID: RefCell<TaskID> = RefCell::new(TaskID(0));
}

fn task_context<F, R>(f: F) -> R
where
    F: FnOnce(TaskID) -> R,
{
    TASK_ID.with(|id| f(*id.borrow()))
}

fn executor_context<F, R>(f: F) -> R
where
    F: FnOnce(&Arc<Executor>) -> R,
{
    EXECUTOR.with(|exec| {
        let exec = exec.borrow();
        let exec = exec
            .as_ref()
            .expect("spawn called outside of an executor context");

        f(exec)
    })
}

pub fn spawn<F>(fut: F) -> SpawnHandle<F::Output>
where
    F: Future + Send + Sync + 'static,
    F::Output: Send + Sync + 'static,
{
    executor_context(|exec| exec.spawn(fut))
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
            task_context(|id| {
                executor_context(|exec| {
                    exec.timers.lock().unwrap().push((Reverse(instant), id));
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
        Sleep::until(Instant::now() + duration).await
    }
}

/// Run a future to completion on the current thread.
pub fn block_on<Fut: Future>(fut: Fut) -> Fut::Output
where
    Fut::Output: Send + Sync + 'static,
    Fut: Send + Sync + 'static,
{
    let executor = Arc::new(Executor::default());
    EXECUTOR.with(|exec| exec.borrow_mut().replace(executor.clone()));

    // spawn a bunch of worker threads
    for i in 1..8 {
        let exec = executor.clone();
        let join_handle = thread::Builder::new()
            .name(format!("tl-async-runtime-worker-{}", i))
            .spawn(move || exec.run_worker_thread())
            .unwrap();

        executor.threads.write().unwrap().push(ThreadWrapper {
            thread: join_handle.thread().clone(),
        })
    }

    executor.threads.write().unwrap().push(ThreadWrapper {
        thread: thread::current(),
    });

    let handle = executor.spawn(fut);
    pin_mut!(handle);

    let waker = Waker::from(Arc::new(ThreadWaker {
        executor: executor.clone(),
        thread: thread::current(),
    }));
    let mut cx = Context::from_waker(&waker);

    // Run the future to completion.
    loop {
        // if the output value is ready, return
        if let Poll::Ready(res) = handle.as_mut().poll(&mut cx) {
            break res;
        }
        executor.run_task();
        // executor.book_keeping();
    }
}

impl Executor {
    fn book_keeping(self: &Arc<Self>) {
        // get the current task timers that have elapsed and insert them into the ready tasks
        let instant = Instant::now();
        let mut timers = self.timers.lock().unwrap();
        while timers.peek().map(|(t, _)| t.0 < instant).unwrap_or(false) {
            let (_, id) = timers.pop().unwrap();
            self.signal_ready(id);
        }

        // // get the OS events
        // let mut os = self.os.lock().unwrap();
        // let Os { poll, events } = &mut *os;
        // poll.poll(events, Some(Duration::from_millis(10)))
        //     .unwrap();

        // for event in &os.events {}
    }

    fn park_thread(self: &Arc<Self>) {
        if Arc::strong_count(&self.parked) < self.threads.read().unwrap().len() {
            let _park = self.parked.clone();
            // println!("parking {:?}", thread::current().id());
            thread::park();
            // println!("resuming {:?}", thread::current().id());
        }
    }

    fn acquire_task(self: &Arc<Self>) -> ControlFlow<(), Option<(TaskID, Task)>> {
        // request a new task from the queue
        let task = loop {
            if self.done.load(Ordering::Relaxed) {
                return ControlFlow::Break(());
            }
            match self.ready.poll() {
                Ok(task) => break task,
                Err(TryRecvError::Empty) => {
                    // if no tasks are available, park the thread.
                    // threads are woken up randomly when new tasks become available
                    self.park_thread();
                    self.book_keeping();
                }
                Err(TryRecvError::Disconnected) => {
                    // queue has closed, this means the block_on main thread has exited
                    return ControlFlow::Break(());
                }
            }
        };

        // the task may have been dropped or acquired by another thread already
        match self.tasks.lock().unwrap().remove(&task) {
            Some(fut) => ControlFlow::Continue(Some((task, fut))),
            None => ControlFlow::Continue(None),
        }
    }

    fn run_task(self: &Arc<Self>) -> ControlFlow<(), Option<()>> {
        // self.book_keeping();

        // acquire sole access to an available task
        let (task, mut fut) = match self.acquire_task()? {
            Some(task) => task,
            None => return ControlFlow::Continue(None),
        };

        TASK_ID.with(|id| {
            *id.borrow_mut() = task;

            let waker = Waker::from(Arc::new(self.clone().new_task_waker(task)));
            let mut cx = Context::from_waker(&waker);

            // poll the fut
            match fut.as_mut().poll(&mut cx) {
                Poll::Ready(()) => {}
                Poll::Pending => {
                    // if the task isn't yet ready, put it back on the task list.
                    // It's the task's responsibility to wake it self up (eg timer queue or IO events)

                    // println!("thread: acquiring lock on tasks 2");
                    self.tasks.lock().unwrap().insert(task, fut);
                    // println!("thread: releasing lock on tasks 2");
                }
            }
        });
        ControlFlow::Continue(Some(()))
    }

    fn run_worker_thread(self: &Arc<Self>) {
        EXECUTOR.with(|exec| *exec.borrow_mut() = Some(self.clone()));

        while let ControlFlow::Continue(_) = self.run_task() {}
    }

    fn new_task_waker(self: Arc<Self>, id: TaskID) -> TaskWaker {
        TaskWaker { id, executor: self }
    }
}
