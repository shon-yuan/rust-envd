//! Shared admission for embedded desktop and user-session helper execution.
//! The canonical secret stays in Configuration; only admission holds its lock.

use super::*;
use crate::configuration::Configuration;
use axum::body::{to_bytes, Body};
use axum::extract::Request;
use tokio::sync::Mutex as AsyncMutex;
use tower::ServiceExt;

type ConfigurationHandle = Arc<AsyncMutex<Configuration>>;

/// Desktop initialization policy, resolved before constructing a registry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LaunchContext {
    DesktopSession,
    ExternalOnly,
}

impl LaunchContext {
    /// SCM attachment and actual session identity are checked independently;
    /// detached service successors remain excluded by Session 0.
    pub fn for_windows(service_attached: bool, session: Option<u32>) -> Self {
        match (service_attached, session) {
            (false, Some(session)) if session != 0 => Self::DesktopSession,
            _ => Self::ExternalOnly,
        }
    }
}

enum Backend {
    Embedded(Router),
    External,
    Unavailable,
}

pub(super) struct Shared {
    configuration: ConfigurationHandle,
    worker: Arc<CuaState>,
    backend: Backend,
    mode: &'static str,
}

impl CuaExtension {
    /// Uses a private MCP router without a socket or process. Production supplies
    /// the reusable worker router; tests cross the same admission seam.
    pub fn embedded(router: Router, configuration: ConfigurationHandle) -> Self {
        let state = bare_state(WorkerStatus::running());
        Self::with_shared(state, configuration, Backend::Embedded(router), "embedded")
    }

    /// Adopts a deployment-managed loopback helper. The token authenticates only
    /// envd-to-helper transport and never substitutes for the live envd token.
    pub async fn adopt_shared(
        listen: SocketAddr,
        configuration: ConfigurationHandle,
        worker_token: Vec<u8>,
    ) -> Result<Self, String> {
        if !listen.ip().is_loopback() {
            return Err("shared cua helper must use a loopback address".into());
        }
        if worker_token.is_empty() || HeaderValue::from_bytes(&worker_token).is_err() {
            return Err("shared cua helper requires a valid nonempty transport token".into());
        }
        let status = match probe(listen).await {
            None => WorkerStatus::running(),
            Some(detail) => WorkerStatus::stopped(detail),
        };
        let state = Arc::new(CuaState {
            status: Mutex::new(status),
            mcp_url: format!("http://{listen}/mcp"),
            proxy: None,
            worker_token: Some(worker_token),
            tool_metrics: Mutex::new(ToolMetrics::default()),
        });
        let watched = state.clone();
        tokio::spawn(async move { watch(listen, watched).await });
        Ok(Self::with_shared(
            state,
            configuration,
            Backend::External,
            "shared-external",
        ))
    }

    fn with_shared(
        state: Arc<CuaState>,
        configuration: ConfigurationHandle,
        backend: Backend,
        mode: &'static str,
    ) -> Self {
        let shared = Arc::new(Shared {
            configuration,
            worker: state.clone(),
            backend,
            mode,
        });
        Self {
            state,
            shared: Some(shared),
        }
    }

    fn shared_unavailable(
        configuration: ConfigurationHandle,
        mode: &'static str,
        detail: &str,
    ) -> Self {
        Self::with_shared(
            bare_state(WorkerStatus::stopped(detail)),
            configuration,
            Backend::Unavailable,
            mode,
        )
    }

    /// Explicit shared modes never fall back to legacy credentials or runtime.
    /// The legacy constructors retain their historical behavior for callers.
    pub async fn from_env_with_context(
        configuration: ConfigurationHandle,
        context: LaunchContext,
    ) -> Option<Self> {
        let mode = std::env::var_os("COORWORKER_CUA_MODE");
        match mode.as_ref().map(|value| value.to_str()) {
            None | Some(Some("legacy")) => {
                Self::from_env_with(context == LaunchContext::DesktopSession).await
            }
            Some(Some("embedded")) => Some(Self::desktop(configuration, context)),
            Some(Some("shared-external")) => Some(Self::external_from_env(configuration).await),
            _ => Some(Self::shared_unavailable(
                configuration,
                "unavailable",
                "invalid COORWORKER_CUA_MODE",
            )),
        }
    }

    fn desktop(configuration: ConfigurationHandle, context: LaunchContext) -> Self {
        if context != LaunchContext::DesktopSession {
            return Self::shared_unavailable(
                configuration,
                "embedded",
                "embedded cua requires a known interactive user session",
            );
        }
        #[cfg(feature = "cua-desktop")]
        {
            Self::embedded(
                coorworker_cua::mcp_router(
                    coorworker_cua::desktop_provider(),
                    coorworker_cua::Auth::disabled(),
                ),
                configuration,
            )
        }
        #[cfg(not(feature = "cua-desktop"))]
        {
            Self::shared_unavailable(
                configuration,
                "embedded",
                "embedded cua requires the cua-desktop build feature",
            )
        }
    }

    async fn external_from_env(configuration: ConfigurationHandle) -> Self {
        let listen =
            std::env::var("COORWORKER_CUA_LISTEN").unwrap_or_else(|_| DEFAULT_LISTEN.into());
        let Ok(listen) = listen.parse() else {
            return Self::shared_unavailable(
                configuration,
                "shared-external",
                "invalid COORWORKER_CUA_LISTEN",
            );
        };
        let token = std::env::var("CUA_ENV_TOKEN")
            .unwrap_or_default()
            .into_bytes();
        match Self::adopt_shared(listen, configuration.clone(), token).await {
            Ok(extension) => extension,
            Err(detail) => Self::shared_unavailable(configuration, "shared-external", &detail),
        }
    }
}

fn bare_state(status: WorkerStatus) -> Arc<CuaState> {
    Arc::new(CuaState {
        status: Mutex::new(status),
        mcp_url: String::new(),
        proxy: None,
        worker_token: None,
        tool_metrics: Mutex::new(ToolMetrics::default()),
    })
}

impl Shared {
    pub(super) fn router(self: &Arc<Self>) -> Router {
        Router::new()
            .route("/mcp", post(mcp))
            .route("/health", get(health))
            .route("/info", get(info))
            .route("/metrics", get(metrics))
            .with_state(self.clone())
    }

    fn available(&self) -> bool {
        self.worker.status.lock().is_ok_and(|status| status.running)
    }

    async fn auth_ready(&self) -> bool {
        self.configuration.lock().await.access.has_token()
    }

    async fn allows(&self, headers: &HeaderMap) -> bool {
        // Reject ambiguous duplicate headers and whitespace-bearing credentials.
        let mut values = headers.get_all(header::AUTHORIZATION).iter();
        let Some(value) = values.next() else {
            return false;
        };
        if values.next().is_some() {
            return false;
        }
        let Some(token) = value
            .to_str()
            .ok()
            .and_then(|value| value.strip_prefix("Bearer "))
        else {
            return false;
        };
        if token.is_empty() || token.bytes().any(|byte| byte.is_ascii_whitespace()) {
            return false;
        }
        self.configuration
            .lock()
            .await
            .access
            .matches_current_token(Some(token))
    }

    async fn dispatch(&self, headers: &HeaderMap, body: Bytes) -> Result<Forwarded, String> {
        match &self.backend {
            Backend::External => forward(&self.worker, headers, body).await,
            Backend::Unavailable => Err("cua runtime unavailable".into()),
            Backend::Embedded(router) => {
                let request = Request::post("/mcp")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(body))
                    .map_err(|_| "cannot build cua request")?;
                let response = router
                    .clone()
                    .oneshot(request)
                    .await
                    .unwrap_or_else(|never| match never {});
                let status = response.status();
                let content_type = response
                    .headers()
                    .get(header::CONTENT_TYPE)
                    .and_then(|value| value.to_str().ok())
                    .map(str::to_owned);
                let body = to_bytes(response.into_body(), PROXY_RESPONSE_LIMIT)
                    .await
                    .map_err(|_| "cua response exceeds limit or could not be read")?;
                Ok(Forwarded {
                    status,
                    content_type,
                    body,
                })
            }
        }
    }
}

async fn mcp(State(state): State<Arc<Shared>>, request: Request) -> Response {
    if !state.allows(request.headers()).await {
        let mut response = error(
            StatusCode::UNAUTHORIZED,
            "missing or invalid envd access token",
        );
        response
            .headers_mut()
            .insert(header::WWW_AUTHENTICATE, HeaderValue::from_static("Bearer"));
        return response;
    }
    if !state.available() {
        return error(StatusCode::SERVICE_UNAVAILABLE, "cua runtime unavailable");
    }
    let (parts, body) = request.into_parts();
    let body = match to_bytes(body, PROXY_REQUEST_LIMIT).await {
        Ok(body) => body,
        Err(_) => {
            return error(
                StatusCode::PAYLOAD_TOO_LARGE,
                "cua request exceeds limit or could not be read",
            )
        }
    };
    let calls = proxied_tool_calls(&body);
    let started = Instant::now();
    let outcome = tokio::time::timeout(PROXY_TIMEOUT, state.dispatch(&parts.headers, body)).await;
    let ok = matches!(&outcome, Ok(Ok(result)) if forwarded_ok(&result.status, &result.body));
    if !calls.is_empty() {
        match state.worker.tool_metrics.lock() {
            Ok(mut metrics) => metrics.record(&calls, ok, started.elapsed()),
            Err(poisoned) => poisoned.into_inner().record(&calls, ok, started.elapsed()),
        }
    }
    match outcome {
        Ok(Ok(result)) => {
            let mut response = Response::new(Body::from(result.body));
            *response.status_mut() = result.status;
            if let Some(value) = result
                .content_type
                .and_then(|value| HeaderValue::from_str(&value).ok())
            {
                response.headers_mut().insert(header::CONTENT_TYPE, value);
            }
            response
        }
        Ok(Err(_)) => error(StatusCode::BAD_GATEWAY, "cua runtime request failed"),
        Err(_) => error(
            StatusCode::GATEWAY_TIMEOUT,
            "cua runtime did not answer in time",
        ),
    }
}

async fn health(State(state): State<Arc<Shared>>) -> Response {
    let available = state.available();
    let auth_ready = state.auth_ready().await;
    let ready = available && auth_ready;
    let detail = state
        .worker
        .status
        .lock()
        .ok()
        .and_then(|status| status.detail.clone());
    (
        if ready {
            StatusCode::OK
        } else {
            StatusCode::SERVICE_UNAVAILABLE
        },
        Json(json!({
            "status": if ready { "healthy" } else { "unhealthy" },
            "mode": state.mode, "runtime_available": available, "auth_ready": auth_ready,
            "ready": ready, "detail": detail, "desktop_access": "not-probed",
        })),
    )
        .into_response()
}

async fn info(State(state): State<Arc<Shared>>) -> Json<serde_json::Value> {
    Json(json!({
        "name": "cua", "mode": state.mode, "transport": "mcp-jsonrpc-http", "protocol": "legacy",
        "auth": "bearer", "auth_source": "envd", "auth_configured": state.auth_ready().await,
        "proxy_path": "/cua/mcp", "proxy_auth": "bearer",
        "helper_transport_auth": matches!(state.backend, Backend::External),
    }))
}

async fn metrics(State(state): State<Arc<Shared>>) -> Response {
    let available = state.available();
    let ready = available && state.auth_ready().await;
    let mut body = super::metrics_text(&state.worker, matches!(state.backend, Backend::External));
    for (name, value) in [("backend_up", available), ("ready", ready)] {
        body.push_str(&format!(
            "# TYPE coorworker_envd_cua_{name} gauge\ncoorworker_envd_cua_{name} {}\n",
            u8::from(value)
        ));
    }
    (
        [(
            header::CONTENT_TYPE,
            "text/plain; version=0.0.4; charset=utf-8",
        )],
        body,
    )
        .into_response()
}
