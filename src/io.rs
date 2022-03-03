pub mod net;

use std::{collections::HashMap, sync::Arc, task::Poll, time::Duration, ops::{Deref, DerefMut}, pin::Pin};

use futures::Future;
use mio::event::Source;
use pin_project::pin_project;
use rand::Rng;

use crate::{
    driver::{executor_context, task_context},
    Executor, TaskId,
};

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
            dbg!(event);
            if let Some(task) = tasks.remove(&event.token()) {
                executor.signal_ready(task);
            }
        }
    }
}

pub(crate) struct Registration<S: Source> {
    pub exec: Arc<Executor>,
    pub token: mio::Token,
    pub source: S,
}

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
        executor_context::<_, std::io::Result<Self>>(|exec| {
            let token = mio::Token(rand::thread_rng().gen());
            exec.os
                .lock()
                .poll
                .registry()
                .register(&mut source, token, interests)?;
            Ok(Self {
                exec: exec.clone(),
                token,
                source,
            })
        })
    }

    pub fn ready(&self) -> RegistrationReady {
        RegistrationReady {
            exec: self.exec.clone(),
            token: Some(self.token),
        }
    }
}

impl<S: Source> Drop for Registration<S> {
    fn drop(&mut self) {
        self.exec
            .os
            .lock()
            .poll
            .registry()
            .deregister(&mut self.source)
            .unwrap();
    }
}

#[pin_project]
pub(crate) struct RegistrationReady {
    pub exec: Arc<Executor>,
    pub token: Option<mio::Token>,
}

impl RegistrationReady {
    fn reset(self: Pin<&mut Self>, token: mio::Token) {
        task_context(|id| {
            self.exec.os.lock().tasks.insert(token, id);
        })
    }
}

impl Future for RegistrationReady {
    type Output = ();

    fn poll(
        mut self: Pin<&mut Self>,
        _: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Self::Output> {
        let this = self.as_mut().project();
        if let Some(token) = this.token.take() {
            self.reset(token);
            Poll::Pending
        } else {
            Poll::Ready(())
        }
    }
}
