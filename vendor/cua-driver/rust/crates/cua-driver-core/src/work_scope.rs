//! Completion tracking for finite native work handed off by one invocation.
//!
//! A bounded driver wait can end before its uncancellable blocking worker. An
//! embedding host uses [`InvocationScope::run`] to retain admission until those
//! workers, including nested jobs and cleanup, actually finish. Intentionally
//! persistent resources (recording loops, browser transports) are not work items.

use std::{
    cell::RefCell,
    future::Future,
    panic::{resume_unwind, AssertUnwindSafe},
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
};

use futures_util::FutureExt;
use tokio::{sync::Notify, task::JoinHandle};

#[derive(Default)]
struct State {
    pending: AtomicUsize,
    changed: Notify,
}

tokio::task_local! {
    static CURRENT: InvocationScope;
}

thread_local! {
    static BLOCKING_CURRENT: RefCell<Option<InvocationScope>> = const { RefCell::new(None) };
}

#[derive(Clone, Default)]
pub struct InvocationScope {
    state: Arc<State>,
}

impl InvocationScope {
    pub fn new() -> Self {
        Self::default()
    }

    /// Bind the invocation while polling, then drain even if its future panics.
    /// The host must keep this future alive after its own response deadline.
    pub async fn run<F: Future>(&self, future: F) -> F::Output {
        let result = CURRENT
            .scope(self.clone(), AssertUnwindSafe(future).catch_unwind())
            .await;
        self.drain().await;
        match result {
            Ok(output) => output,
            Err(panic) => resume_unwind(panic),
        }
    }

    pub async fn drain(&self) {
        loop {
            let notified = self.state.changed.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if self.state.pending.load(Ordering::Acquire) == 0 {
                return;
            }
            notified.await;
        }
    }
}

fn current() -> Option<InvocationScope> {
    CURRENT
        .try_with(Clone::clone)
        .ok()
        .or_else(|| BLOCKING_CURRENT.with(|current| current.borrow().clone()))
}

struct Completion(Option<InvocationScope>);

impl Drop for Completion {
    fn drop(&mut self) {
        if let Some(scope) = &self.0 {
            if scope.state.pending.fetch_sub(1, Ordering::AcqRel) == 1 {
                scope.state.changed.notify_waiters();
            }
        }
    }
}

struct Binding(Option<InvocationScope>);

impl Drop for Binding {
    fn drop(&mut self) {
        BLOCKING_CURRENT.with(|current| *current.borrow_mut() = self.0.take());
    }
}

/// Capture completion before enqueueing and bind the worker for nested handoffs.
/// Also usable with a finite `std::thread::Builder::spawn` cleanup operation.
/// Dropping an unstarted closure releases its marker; worker unwind restores TLS.
pub fn bind_current<F, R>(work: F) -> impl FnOnce() -> R + Send + 'static
where
    F: FnOnce() -> R + Send + 'static,
    R: Send + 'static,
{
    let scope = current();
    if let Some(scope) = &scope {
        scope.state.pending.fetch_add(1, Ordering::AcqRel);
    }
    let completion = Completion(scope);
    move || {
        let previous = BLOCKING_CURRENT.with(|current| current.replace(completion.0.clone()));
        let _binding = Binding(previous);
        // Hold the marker across all native work and nested handoffs, including unwind.
        let _completion = completion;
        work()
    }
}

/// Capture completion before enqueueing a finite future on a chosen runtime.
/// Dropping the future before its first poll releases the marker. Polling binds
/// the scope so nested handoffs inherit it, including across runtime boundaries.
pub fn bind_future<F>(future: F) -> impl Future<Output = F::Output> + Send + 'static
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    let scope = current();
    if let Some(scope) = &scope {
        scope.state.pending.fetch_add(1, Ordering::AcqRel);
    }
    let completion = Completion(scope.clone());
    async move {
        let _completion = completion;
        match scope {
            Some(scope) => CURRENT.scope(scope, future).await,
            None => future.await,
        }
    }
}

/// Spawn finite asynchronous cleanup or child dispatch with scope inheritance.
/// Persistent transport readers and session-lifetime resources must use their
/// ordinary spawner so they cannot keep an invocation open indefinitely.
pub fn spawn<F>(future: F) -> JoinHandle<F::Output>
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    tokio::spawn(bind_future(future))
}

/// Tokio's blocking spawn with optional invocation completion tracking.
/// Outside a scope it preserves the ordinary spawn behavior.
pub fn spawn_blocking<F, R>(work: F) -> JoinHandle<R>
where
    F: FnOnce() -> R + Send + 'static,
    R: Send + 'static,
{
    tokio::task::spawn_blocking(bind_current(work))
}
