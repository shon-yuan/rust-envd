//! Platform-neutral process runtime seam consumed by the shared Connect
//! adapter (`crate::rpc::process`).
//!
//! The adapter owns every wire decision; a platform provides one
//! [`ProcessRuntimeApi`] object. Linux forwards to
//! `crate::platform::linux::process::ProcessRuntime`; the Windows runtime
//! implements the same trait over its own registry in a later slice.

use crate::configuration::Configuration;
use crate::exec_context::{Account, Defaults, Environment};
use crate::logs::Logger;
use crate::process_contract::RuntimeError;
use crate::proto::process as pb;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;

/// A boxed future tied to the borrows its operation needs. Trait objects can
/// carry this without becoming generic.
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// The per-request execution context `configured()` resolves before a
/// process operation: the configuration's defaults and environment plus the
/// authenticated account.
#[derive(Clone, Debug)]
pub struct RuntimeContext {
    pub defaults: Defaults,
    pub environment: Environment,
    pub account: Account,
}

/// The process operations the Connect adapter drives. Implementations are
/// cheap handles: [`with_context`](Self::with_context) and
/// [`with_account`](Self::with_account) return a new handle sharing the same
/// underlying process registry.
pub trait ProcessRuntimeApi: Send + Sync + 'static {
    fn with_context(
        &self,
        defaults: Defaults,
        environment: Environment,
    ) -> Arc<dyn ProcessRuntimeApi>;
    fn with_account(&self, account: Account) -> Arc<dyn ProcessRuntimeApi>;
    fn list(&self) -> BoxFuture<'_, Vec<pb::ProcessInfo>>;
    fn start(
        &self,
        request: pb::StartRequest,
        deadline: Option<Duration>,
        operation_id: Option<String>,
    ) -> BoxFuture<'_, Result<Box<dyn ProcessSubscription>, RuntimeError>>;
    fn connect<'a>(
        &'a self,
        selector: &'a pb::ProcessSelector,
    ) -> BoxFuture<'a, Result<Box<dyn ProcessSubscription>, RuntimeError>>;
    fn select_input<'a>(
        &'a self,
        selector: &'a pb::ProcessSelector,
    ) -> BoxFuture<'a, Result<Box<dyn SelectedInput>, RuntimeError>>;
    fn send_input<'a>(
        &'a self,
        selector: &'a pb::ProcessSelector,
        input: &'a pb::ProcessInput,
    ) -> BoxFuture<'a, Result<(), RuntimeError>>;
    fn close_stdin<'a>(
        &'a self,
        selector: &'a pb::ProcessSelector,
    ) -> BoxFuture<'a, Result<(), RuntimeError>>;
    fn send_signal<'a>(
        &'a self,
        selector: &'a pb::ProcessSelector,
        signal: i32,
    ) -> BoxFuture<'a, Result<(), RuntimeError>>;
    fn update<'a>(
        &'a self,
        request: &'a pb::UpdateRequest,
    ) -> BoxFuture<'a, Result<(), RuntimeError>>;
}

/// A live event subscription. Dropping it disconnects the observer, never the
/// child.
pub trait ProcessSubscription: Send {
    fn recv(&mut self) -> BoxFuture<'_, Option<pb::ProcessEvent>>;
}

/// A selector resolved once, retaining the selected process identity across
/// tag or PID reuse for the life of a StreamInput call.
pub trait SelectedInput: Send + Sync {
    fn send_input<'a>(
        &'a self,
        input: &'a pb::ProcessInput,
    ) -> BoxFuture<'a, Result<(), RuntimeError>>;
}

/// The daemon state the shared adapter reads besides the runtime object.
pub trait ProcessRpcState: Send + Sync + 'static {
    fn configuration(&self) -> &Arc<Mutex<Configuration>>;
    fn process_logger(&self) -> &Logger;
}

#[cfg(target_os = "linux")]
mod linux {
    use super::{
        pb, Account, Arc, BoxFuture, Defaults, Duration, Environment, ProcessRuntimeApi,
        ProcessSubscription, RuntimeError, SelectedInput,
    };
    use crate::platform::linux::process::{ProcessRuntime, SelectedProcess, Subscription};

    impl ProcessRuntimeApi for ProcessRuntime {
        fn with_context(
            &self,
            defaults: Defaults,
            environment: Environment,
        ) -> Arc<dyn ProcessRuntimeApi> {
            Arc::new(ProcessRuntime::with_context(self, defaults, environment))
        }

        fn with_account(&self, account: Account) -> Arc<dyn ProcessRuntimeApi> {
            Arc::new(ProcessRuntime::with_account(self, account))
        }

        fn list(&self) -> BoxFuture<'_, Vec<pb::ProcessInfo>> {
            Box::pin(ProcessRuntime::list(self))
        }

        fn start(
            &self,
            request: pb::StartRequest,
            deadline: Option<Duration>,
            operation_id: Option<String>,
        ) -> BoxFuture<'_, Result<Box<dyn ProcessSubscription>, RuntimeError>> {
            Box::pin(async move {
                let subscription = self
                    .start_with_operation_id(request, deadline, operation_id)
                    .await?;
                Ok(Box::new(subscription) as Box<dyn ProcessSubscription>)
            })
        }

        fn connect<'a>(
            &'a self,
            selector: &'a pb::ProcessSelector,
        ) -> BoxFuture<'a, Result<Box<dyn ProcessSubscription>, RuntimeError>> {
            Box::pin(async move {
                let subscription = ProcessRuntime::connect(self, selector).await?;
                Ok(Box::new(subscription) as Box<dyn ProcessSubscription>)
            })
        }

        fn select_input<'a>(
            &'a self,
            selector: &'a pb::ProcessSelector,
        ) -> BoxFuture<'a, Result<Box<dyn SelectedInput>, RuntimeError>> {
            Box::pin(async move {
                let selected = ProcessRuntime::select_input(self, selector).await?;
                Ok(Box::new(selected) as Box<dyn SelectedInput>)
            })
        }

        fn send_input<'a>(
            &'a self,
            selector: &'a pb::ProcessSelector,
            input: &'a pb::ProcessInput,
        ) -> BoxFuture<'a, Result<(), RuntimeError>> {
            Box::pin(ProcessRuntime::send_input(self, selector, input))
        }

        fn close_stdin<'a>(
            &'a self,
            selector: &'a pb::ProcessSelector,
        ) -> BoxFuture<'a, Result<(), RuntimeError>> {
            Box::pin(ProcessRuntime::close_stdin(self, selector))
        }

        fn send_signal<'a>(
            &'a self,
            selector: &'a pb::ProcessSelector,
            signal: i32,
        ) -> BoxFuture<'a, Result<(), RuntimeError>> {
            Box::pin(ProcessRuntime::send_signal(self, selector, signal))
        }

        fn update<'a>(
            &'a self,
            request: &'a pb::UpdateRequest,
        ) -> BoxFuture<'a, Result<(), RuntimeError>> {
            Box::pin(ProcessRuntime::update(self, request))
        }
    }

    impl ProcessSubscription for Subscription {
        fn recv(&mut self) -> BoxFuture<'_, Option<pb::ProcessEvent>> {
            Box::pin(Subscription::recv(self))
        }
    }

    impl SelectedInput for SelectedProcess {
        fn send_input<'a>(
            &'a self,
            input: &'a pb::ProcessInput,
        ) -> BoxFuture<'a, Result<(), RuntimeError>> {
            Box::pin(SelectedProcess::send_input(self, input))
        }
    }
}
