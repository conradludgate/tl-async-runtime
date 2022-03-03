use crossbeam_channel::{unbounded, Receiver, Sender, TryRecvError};
use futures::channel::oneshot;
use futures::FutureExt;
use pin_project::pin_project;
use rand::Rng;
use std::cell::RefCell;
use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap};
use std::future::Future;
use std::ops::ControlFlow;
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
    fn get_ready_task(self: &Arc<Self>) -> Result<TaskID, TryRecvError> {
        // println!("getting task {:?}", thread::current().id());
        self.ready_reciever.try_recv()
    }

    fn signal_ready(self: &Arc<Self>, id: TaskID) {
        self.ready_sender.send(id).unwrap();

        let mut threads = self.threads.write().unwrap();
        threads.rotate_left(1);
        threads[0].thread.thread().unpark();
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

pub struct MyWaker {
    executor: Arc<Executor>,
    id: TaskID,
}

impl Wake for MyWaker {
    fn wake(self: Arc<Self>) {
        self.wake_by_ref();
    }
    fn wake_by_ref(self: &Arc<Self>) {
        self.executor.signal_ready(self.id);
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
                    exec.timers.write().unwrap().push((Reverse(instant), id));
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
    let (ready_sender, ready_reciever) = unbounded::<TaskID>();
    let executor = Arc::new(Executor {
        threads: RwLock::new(Vec::new()),
        tasks: Mutex::new(HashMap::new()),
        timers: RwLock::new(BinaryHeap::new()),
        ready_reciever,
        ready_sender,
    });

    EXECUTOR.with(|exec| exec.borrow_mut().replace(executor.clone()));

    // spawn a bunch fof worker threads
    for i in 0..7 {
        let exec = executor.clone();
        let thread = thread::Builder::new()
            .name(format!("tl-async-runtime-worker-{}", i))
            .spawn(move || exec.run_worker_thread())
            .unwrap();

        executor
            .threads
            .write()
            .unwrap()
            .push(ThreadWrapper { thread })
    }

    let mut handle = executor.spawn(fut);

    // Run the future to completion.
    loop {
        // if the output value is ready, return
        if let Some(res) = handle.receiver.try_recv().unwrap() {
            break res;
        }
        // get the current task timers that have elapsed and insert them into the ready tasks
        let instant = Instant::now();
        let mut timers = executor.timers.write().unwrap();
        while timers.peek().map(|(t, _)| t.0 < instant).unwrap_or(false) {
            let (_, id) = timers.pop().unwrap();
            executor.signal_ready(id);
        }
    }
}

impl Executor {
    fn acquire_task(self: &Arc<Self>) -> ControlFlow<(), Option<(TaskID, Task)>> {
        // request a new task from the queue
        let task = loop {
            match self.get_ready_task() {
                Ok(task) => break task,
                Err(TryRecvError::Empty) => {
                    // if no tasks are available, park the thread.
                    // threads are woken up randomly when new tasks become available

                    // println!("parking {:?}", thread::current().id());
                    thread::park();
                    // println!("resuming {:?}", thread::current().id());
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

    fn run_worker_thread(self: &Arc<Self>) {
        EXECUTOR.with(|exec| *exec.borrow_mut() = Some(self.clone()));

        loop {
            // acquire sole access to an available task
            let (task, mut fut) = match self.acquire_task() {
                ControlFlow::Continue(Some(task)) => task,
                ControlFlow::Continue(None) => continue,
                ControlFlow::Break(()) => return,
            };

            TASK_ID.with(|id| {
                *id.borrow_mut() = task;

                let waker = Waker::from(Arc::new(self.clone().new_waker(task)));
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
        }
    }

    fn new_waker(self: Arc<Self>, id: TaskID) -> MyWaker {
        MyWaker { id, executor: self }
    }
}
