use crossbeam_channel::{unbounded, Receiver, Sender, TryRecvError};

use crate::Task;

pub(crate) struct Queue {
    pub receiver: Receiver<Task>,
    pub sender: Sender<Task>,
}

impl Queue {
    pub fn poll(&self) -> Result<Task, TryRecvError> {
        self.receiver.try_recv()
    }
    pub fn push(&self, task: Task) {
        self.sender.send(task).unwrap();
    }
}

impl Default for Queue {
    fn default() -> Self {
        let (sender, receiver) = unbounded();
        Self { receiver, sender }
    }
}
