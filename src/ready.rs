use crossbeam_channel::{unbounded, Receiver, Sender, TryRecvError};

use crate::TaskId;

pub(crate) struct Queue {
    pub receiver: Receiver<TaskId>,
    pub sender: Sender<TaskId>,
}

impl Queue {
    pub fn poll(&self) -> Result<TaskId, TryRecvError> {
        self.receiver.try_recv()
    }
    pub fn push(&self, id: TaskId) {
        self.sender.send(id).unwrap();
    }
}

impl Default for Queue {
    fn default() -> Self {
        let (sender, receiver) = unbounded();
        Self { receiver, sender }
    }
}
