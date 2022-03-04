/// Networking specific handlers
pub mod net;

use crate::{driver::executor_context, Executor};
use futures::channel::mpsc::{unbounded, UnboundedReceiver, UnboundedSender};
use mio::event::Source;
use rand::Rng;
use std::{
    collections::HashMap,
    ops::{Deref, DerefMut},
    sync::Arc,
    time::Duration,
};

#[derive(Debug)]
pub(crate) struct Event(u8);
impl Event {
    pub fn is_readable(&self) -> bool {
        self.0 & 1 != 0
    }
    pub fn is_writable(&self) -> bool {
        self.0 & 2 != 0
    }
}

impl From<&mio::event::Event> for Event {
    fn from(e: &mio::event::Event) -> Self {
        let mut event = 0;
        event |= (e.is_readable() as u8) << 1;
        event |= (e.is_writable() as u8) << 2;
        Event(event)
    }
}

pub(crate) struct Os {
    pub poll: mio::Poll,
    pub events: mio::Events,
    pub tasks: HashMap<mio::Token, UnboundedSender<Event>>,
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
    /// Polls the OS for new events, and dispatches those to any awaiting tasks
    pub(crate) fn process(&mut self) {
        let Self {
            poll,
            events,
            tasks,
        } = self;
        poll.poll(events, Some(Duration::from_millis(10))).unwrap();

        for event in &*events {
            if let Some(sender) = tasks.get(&event.token()) {
                sender.unbounded_send(event.into()).unwrap();
            }
        }
    }
}

pub(crate) struct Registration<S: Source> {
    pub exec: Arc<Executor>,
    pub token: mio::Token,
    pub source: S,
}

// allow internal access to the source
impl<S: Source> Deref for Registration<S> {
    type Target = S;

    fn deref(&self) -> &Self::Target {
        &self.source
    }
}
impl<S: Source> DerefMut for Registration<S> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.source
    }
}

impl<S: Source> Registration<S> {
    pub fn new(mut source: S, interests: mio::Interest) -> std::io::Result<Self> {
        executor_context(|exec| {
            let token = mio::Token(rand::thread_rng().gen());
            let os = exec.os.lock();
            os.poll.registry().register(&mut source, token, interests)?;
            Ok(Self {
                exec: exec.clone(),
                token,
                source,
            })
        })
    }

    // register this token on the event dispatcher
    // and return a receiver to it
    pub fn events(&self) -> UnboundedReceiver<Event> {
        let (sender, receiver) = unbounded();
        self.exec.os.lock().tasks.insert(self.token, sender);
        receiver
    }
}

impl<S: Source> Drop for Registration<S> {
    fn drop(&mut self) {
        let mut os = self.exec.os.lock();
        // deregister the source from the OS
        os.poll.registry().deregister(&mut self.source).unwrap();
        // remove the event dispatcher
        os.tasks.remove(&self.token);
    }
}
