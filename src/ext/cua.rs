//! The `cua` extension: supervision and discovery for the computer-use worker.
//!
//! The worker is a separate process by construction (see
//! [`coorworker_cua`](https://github.com/shon-yuan/rust-envd)): it links the
//! desktop stack that the frozen envd daemon must not carry, and on Windows it
//! must run in the interactive session rather than in the service's session 0.
//! This extension therefore does not link Cua Driver. It spawns the worker,
//! answers liveness for it, and publishes the endpoint a gateway or ToolHost
//! should dial.
//!
//! Enabled with `COORWORKER_CUA_BIN`; the listen address defaults to
//! `127.0.0.1:3211` and can be set with `COORWORKER_CUA_LISTEN`. A worker that
//! fails to start leaves the extension unhealthy and visible, and never
//! affects the envd surface.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::{
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::get,
    Json, Router,
};
use serde_json::json;
use tokio::process::{Child, Command};

use super::{Extension, Health};

const DEFAULT_LISTEN: &str = "127.0.0.1:3211";
const READY_TIMEOUT: Duration = Duration::from_secs(5);
const READY_POLL: Duration = Duration::from_millis(50);

struct Worker {
    child: Option<Child>,
    error: Option<String>,
}

impl Worker {
    fn health(&mut self) -> Health {
        if let Some(error) = &self.error {
            return Health::unhealthy(error.clone());
        }
        let Some(child) = self.child.as_mut() else {
            return Health::unhealthy("worker is not running");
        };
        match child.try_wait() {
            Ok(None) => Health::healthy(),
            Ok(Some(status)) => Health::unhealthy(format!("worker exited with {status}")),
            Err(error) => Health::unhealthy(format!("cannot read worker status: {error}")),
        }
    }
}

struct CuaState {
    worker: Mutex<Worker>,
    mcp_url: String,
}

/// Supervises one `coorworker-cua` process.
pub struct CuaExtension {
    state: Arc<CuaState>,
}

impl CuaExtension {
    /// Spawns the worker and waits until its loopback listener accepts, up to
    /// five seconds. A worker that cannot start is reported through
    /// [`Extension::health`] instead of aborting the daemon.
    pub async fn spawn(binary: PathBuf, listen: SocketAddr) -> Self {
        let mcp_url = format!("http://{listen}/mcp");
        let worker = match launch(&binary, listen).await {
            Ok(child) => Worker {
                child: Some(child),
                error: None,
            },
            Err(error) => Worker {
                child: None,
                error: Some(error),
            },
        };
        Self {
            state: Arc::new(CuaState {
                worker: Mutex::new(worker),
                mcp_url,
            }),
        }
    }

    /// `Some` when `COORWORKER_CUA_BIN` is set. The value `None` is the parity
    /// build: no extension, no routes, no child.
    pub async fn from_env() -> Option<Self> {
        let binary = std::env::var_os("COORWORKER_CUA_BIN")?;
        let listen = match std::env::var("COORWORKER_CUA_LISTEN") {
            Ok(value) => match value.parse() {
                Ok(listen) => listen,
                Err(error) => {
                    return Some(Self::unavailable(format!(
                        "invalid COORWORKER_CUA_LISTEN {value:?}: {error}"
                    )))
                }
            },
            Err(_) => DEFAULT_LISTEN.parse().expect("default listen parses"),
        };
        Some(Self::spawn(PathBuf::from(binary), listen).await)
    }

    /// An extension that is registered but cannot start. Keeping it visible
    /// is the point: `/cua/health` says why instead of the surface vanishing.
    pub fn unavailable(reason: impl Into<String>) -> Self {
        Self {
            state: Arc::new(CuaState {
                worker: Mutex::new(Worker {
                    child: None,
                    error: Some(reason.into()),
                }),
                mcp_url: format!("http://{DEFAULT_LISTEN}/mcp"),
            }),
        }
    }

    pub fn mcp_url(&self) -> &str {
        &self.state.mcp_url
    }
}

async fn launch(binary: &PathBuf, listen: SocketAddr) -> Result<Child, String> {
    let mut child = Command::new(binary)
        .arg("--listen")
        .arg(listen.to_string())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .kill_on_drop(true)
        .spawn()
        .map_err(|error| format!("cannot spawn {}: {error}", binary.display()))?;

    let deadline = Instant::now() + READY_TIMEOUT;
    loop {
        match tokio::net::TcpStream::connect(listen).await {
            Ok(_) => return Ok(child),
            Err(error) if Instant::now() >= deadline => {
                let _ = child.kill().await;
                return Err(format!("worker did not accept on {listen}: {error}"));
            }
            Err(_) => {
                if let Ok(Some(status)) = child.try_wait() {
                    return Err(format!("worker exited during startup with {status}"));
                }
                tokio::time::sleep(READY_POLL).await;
            }
        }
    }
}

impl Extension for CuaExtension {
    fn name(&self) -> &str {
        "cua"
    }

    fn router(&self) -> Router {
        Router::new()
            .route("/health", get(health))
            .route("/info", get(info))
            .with_state(Arc::clone(&self.state))
    }

    fn health(&self) -> Health {
        match self.state.worker.lock() {
            Ok(mut worker) => worker.health(),
            Err(_) => Health::unhealthy("worker state is poisoned"),
        }
    }
}

async fn health(State(state): State<Arc<CuaState>>) -> Response {
    let health = match state.worker.lock() {
        Ok(mut worker) => worker.health(),
        Err(_) => Health::unhealthy("worker state is poisoned"),
    };
    let status = match health.state {
        super::HealthState::Healthy => StatusCode::OK,
        super::HealthState::Degraded | super::HealthState::Unhealthy => {
            StatusCode::SERVICE_UNAVAILABLE
        }
    };
    (
        status,
        Json(json!({
            "status": match health.state {
                super::HealthState::Healthy => "healthy",
                super::HealthState::Degraded => "degraded",
                super::HealthState::Unhealthy => "unhealthy",
            },
            "detail": health.detail,
            "mcp": state.mcp_url,
        })),
    )
        .into_response()
}

async fn info(State(state): State<Arc<CuaState>>) -> Response {
    // The token is never echoed: a discovery document that leaks it would
    // hand every guest process control of the desktop.
    Json(json!({
        "mcp_url": state.mcp_url,
        "transport": "mcp-jsonrpc-http",
        "protocol": "legacy",
    }))
    .into_response()
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::ext::Extensions;
    use axum::body::{to_bytes, Body};
    use axum::http::Request;
    use tower::ServiceExt;

    fn free_listen() -> SocketAddr {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        drop(listener);
        address
    }

    async fn worker_that_stays() -> Child {
        Command::new("sh")
            .arg("-c")
            .arg("sleep 30")
            .kill_on_drop(true)
            .spawn()
            .unwrap()
    }

    #[tokio::test]
    async fn a_running_worker_is_healthy_and_discoverable() {
        let extension = CuaExtension {
            state: Arc::new(CuaState {
                worker: Mutex::new(Worker {
                    child: Some(worker_that_stays().await),
                    error: None,
                }),
                mcp_url: "http://127.0.0.1:1/mcp".to_owned(),
            }),
        };
        assert_eq!(extension.health(), Health::healthy());
        assert_eq!(extension.mcp_url(), "http://127.0.0.1:1/mcp");

        let extensions = Extensions::new(vec![Arc::new(extension)]).unwrap();
        let router = extensions.mount(Router::new());

        let response = router
            .clone()
            .oneshot(Request::get("/cua/health").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let body: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(body["status"], "healthy");
        assert_eq!(body["mcp"], "http://127.0.0.1:1/mcp");

        let response = router
            .oneshot(Request::get("/cua/info").body(Body::empty()).unwrap())
            .await
            .unwrap();
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        assert!(!String::from_utf8_lossy(&body).contains("token"));
    }

    #[tokio::test]
    async fn a_failed_worker_is_unhealthy_not_fatal() {
        let extension = CuaExtension {
            state: Arc::new(CuaState {
                worker: Mutex::new(Worker {
                    child: None,
                    error: Some("cannot spawn /missing".to_owned()),
                }),
                mcp_url: "http://127.0.0.1:1/mcp".to_owned(),
            }),
        };
        assert_eq!(
            extension.health(),
            Health::unhealthy("cannot spawn /missing")
        );
    }

    #[tokio::test]
    async fn spawn_reports_a_missing_binary() {
        let extension = CuaExtension::spawn(
            PathBuf::from("/definitely/not/coorworker-cua"),
            free_listen(),
        )
        .await;
        assert_eq!(extension.health().state, crate::ext::HealthState::Unhealthy);
    }
}
