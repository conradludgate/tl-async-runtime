#![forbid(unsafe_code)]

#[macro_use]
extern crate educe;

use executor::{context, Executor};
use std::future::Future;
use std::sync::Arc;

mod executor;
mod reactor;
mod ready;

/// Networking specific handlers
pub mod net;
pub use executor::JoinHandle;
pub use reactor::timers::Sleep;

/// Spawn a future on the current runtime.
/// Returns a new future that can be later awaited for it's output.
/// Task execution begins eagerly, without needing you to await it
pub fn spawn<F, R>(fut: F) -> JoinHandle<R>
where
    F: Future<Output = R> + Send + Sync + 'static,
    R: Send + Sync + 'static,
{
    context(|exec| exec.clone().spawn(fut))
}

/// Run a future to completion.
///
/// Starts a new runtime and spawns the future on it.
pub fn block_on<F, R>(fut: F) -> R
where
    F: Future<Output = R> + Send + Sync + 'static,
    R: Send + Sync + 'static,
{
    Arc::new(Executor::default()).block_on(fut)
}
