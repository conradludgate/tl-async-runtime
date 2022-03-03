use std::{collections::HashMap, time::Duration};

use crate::{Executor, TaskId};

pub(crate) struct Os {
    pub poll: mio::Poll,
    pub events: mio::Events,
    pub tasks: HashMap<mio::Token, TaskId>,
}

impl Default for Os {
    fn default() -> Self {
        Self {
            poll: mio::Poll::new().unwrap(),
            events: mio::Events::with_capacity(128),
            tasks: HashMap::new(),
        }
    }
}

impl Os {
    pub(crate) fn process(&mut self, executor: &Executor) {
        let Self {
            poll,
            events,
            tasks,
        } = self;
        poll.poll(events, Some(Duration::from_millis(10))).unwrap();

        for event in &*events {
            if let Some(task) = tasks.get(&event.token()) {
                executor.signal_ready(*task);
            }
        }
    }
}
