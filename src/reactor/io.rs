use crate::{executor::context, Executor};
use chashmap::CHashMap;
use futures::channel::mpsc::{unbounded, UnboundedReceiver, UnboundedSender};
use mio::event::Source;
use parking_lot::RwLock;
use rand::Rng;
use std::{
    ops::{Deref, DerefMut},
    sync::Arc,
    time::Duration,
};

#[derive(Debug)]
pub(crate) struct Event(u8);
impl Event {
    pub fn is_readable(&self) -> bool {
        self.0 & 1 != 0 || self.is_read_closed()
    }
    pub fn is_writable(&self) -> bool {
        self.0 & 2 != 0 || self.is_write_closed()
    }
    pub fn is_read_closed(&self) -> bool {
        self.0 & 4 != 0
    }
    pub fn is_write_closed(&self) -> bool {
        self.0 & 8 != 0
    }
}

impl From<&mio::event::Event> for Event {
    fn from(e: &mio::event::Event) -> Self {
        let mut event = 0;
        event |= (e.is_readable() as u8) << 1;
        event |= (e.is_writable() as u8) << 2;
        event |= (e.is_read_closed() as u8) << 3;
        event |= (e.is_write_closed() as u8) << 4;
        Event(event)
    }
}

pub(crate) struct Os {
    pub poll: RwLock<mio::Poll>,
    pub events: RwLock<mio::Events>,
    pub tasks: CHashMap<mio::Token, UnboundedSender<Event>>,
}

impl Default for Os {
    fn default() -> Self {
        Self {
            poll: RwLock::new(mio::Poll::new().unwrap()),
            events: RwLock::new(mio::Events::with_capacity(128)),
            tasks: CHashMap::new(),
        }
    }
}

impl Os {
    /// Polls the OS for new events, and dispatches those to any awaiting tasks
    pub(crate) fn process(&self) -> usize {
        let mut n = 0;

        self.poll
            .write()
            .poll(&mut self.events.write(), Some(Duration::from_micros(100)))
            .unwrap();

        let mut remove = vec![];

        for event in &*self.events.read() {
            if let Some(sender) = self.tasks.get(&event.token()) {
                if sender.unbounded_send(event.into()).is_err() {
                    n += 1;
                    remove.push(event.token());
                }
            }
        }

        for token in remove {
            self.tasks.remove(&token);
        }

        n
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
        context(|exec| {
            let token = mio::Token(rand::thread_rng().gen());
            let poll = exec.reactor.os.poll.read();
            poll.registry().register(&mut source, token, interests)?;
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
        self.exec.reactor.os.tasks.insert(self.token, sender);
        receiver
    }
}

impl<S: Source> Drop for Registration<S> {
    fn drop(&mut self) {
        // deregister the source from the OS
        let poll = self.exec.reactor.os.poll.read();
        poll.registry().deregister(&mut self.source).unwrap();
        // remove the event dispatcher
        self.exec.reactor.os.tasks.remove(&self.token);
    }
}
