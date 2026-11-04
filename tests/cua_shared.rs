//! Shared CUA admission through the real daemon /init and S0 middleware.
//! The only proposed Rust APIs are CuaExtension::{embedded, adopt_shared}.
//! Health reports runtime_available and auth_ready separately; embedded discovery
//! uses /cua/mcp and never invents a helper endpoint or process lifecycle.
#![cfg(target_os = "linux")]

use std::{
    net::SocketAddr,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};

use axum::{
    body::{to_bytes, Body, Bytes},
    extract::State,
    http::{header, HeaderMap, HeaderValue, Request, StatusCode},
    response::Response,
    routing::{get as route_get, post},
    Json, Router,
};
use coorworker_envd::{
    configuration::Configuration,
    daemon::{app_with_extensions, Daemon},
    ext::{cua::CuaExtension, Extension, Extensions},
    http::ConfigurationState,
    mmds::{BoxFuture, TokenHashProvider},
};
use serde_json::{json, Value};
use sha2::{Digest, Sha512};
use tokio::sync::{Mutex as AsyncMutex, Notify};
use tower::ServiceExt;

const CALL: &str =
    r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"probe","arguments":{}}}"#;
const WORKER_TOKEN: &[u8] = b"private-helper-credential";
const WAIT: Duration = Duration::from_secs(2);

#[derive(Clone, Copy, Debug)]
enum Adapter {
    Embedded,
    SharedExternal,
}
const ADAPTERS: [Adapter; 2] = [Adapter::Embedded, Adapter::SharedExternal];

#[derive(Default)]
struct Provider(Mutex<Option<String>>);

impl Provider {
    fn set(&self, token: &str) {
        *self.0.lock().unwrap() = Some(format!("{:x}", Sha512::digest(token.as_bytes())));
    }
}

impl TokenHashProvider for Provider {
    fn token_hash(&self) -> BoxFuture<'_, std::io::Result<String>> {
        let hash = self.0.lock().unwrap().clone();
        Box::pin(async move { hash.ok_or_else(|| std::io::Error::other("unavailable")) })
    }
}

#[derive(Default)]
struct Backend {
    seen: Mutex<Vec<(HeaderMap, Bytes)>>,
    block_next: AtomicBool,
    entered: Notify,
    release: Notify,
}

impl Backend {
    fn calls(&self) -> usize {
        self.seen.lock().unwrap().len()
    }

    fn router(self: &Arc<Self>) -> Router {
        Router::new()
            .route("/health", route_get(|| async { StatusCode::NO_CONTENT }))
            .route("/mcp", post(backend))
            .with_state(self.clone())
    }
}

async fn backend(
    State(backend): State<Arc<Backend>>,
    headers: HeaderMap,
    body: Bytes,
) -> Json<Value> {
    backend.seen.lock().unwrap().push((headers, body));
    if backend.block_next.swap(false, Ordering::SeqCst) {
        backend.entered.notify_one();
        backend.release.notified().await;
    }
    Json(json!({"jsonrpc":"2.0","id":1,"result":{"content":[]}}))
}

struct Server(tokio::task::JoinHandle<()>);
impl Drop for Server {
    fn drop(&mut self) {
        self.0.abort();
    }
}

async fn serve(router: Router) -> (SocketAddr, Server) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = Server(tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    }));
    (address, server)
}

struct Fixture {
    router: Router,
    provider: Arc<Provider>,
    configuration: Arc<AsyncMutex<Configuration>>,
    backend: Arc<Backend>,
    _server: Option<Server>,
}

impl Fixture {
    async fn new(adapter: Adapter, handover: bool) -> Self {
        let provider = Arc::new(Provider::default());
        // Fixed correction clock makes the stale /init case deterministic.
        // No guest services or clock setter are installed.
        let mut daemon =
            Daemon::with_token_hash_provider(provider.clone()).with_now(Arc::new(|| 0));
        if handover {
            daemon = daemon.with_handover_auth().await;
        }
        let daemon = Arc::new(daemon);
        let configuration = daemon.configuration().clone();
        let backend = Arc::new(Backend::default());
        let (extension, server): (CuaExtension, Option<Server>) = match adapter {
            Adapter::Embedded => (
                CuaExtension::embedded(backend.router(), configuration.clone()),
                None,
            ),
            Adapter::SharedExternal => {
                let (address, server) = serve(backend.router()).await;
                let extension = CuaExtension::adopt_shared(
                    address,
                    configuration.clone(),
                    WORKER_TOKEN.to_vec(),
                )
                .await
                .expect("adopt private healthy helper");
                (extension, Some(server))
            }
        };
        let extensions = Extensions::new(vec![Arc::new(extension) as Arc<dyn Extension>]).unwrap();
        Self {
            router: app_with_extensions(daemon, &extensions),
            provider,
            configuration,
            backend,
            _server: server,
        }
    }

    async fn install(&self, token: &str) {
        self.provider.set(token);
        init(
            &self.router,
            json!({"accessToken": token}),
            StatusCode::NO_CONTENT,
        )
        .await;
    }

    async fn admission(&self, token: Option<&str>, expected: StatusCode) {
        let before = self.backend.calls();
        let response = send(&self.router, mcp_request(token)).await;
        assert_eq!(response.status(), expected, "token {token:?}");
        assert_eq!(
            self.backend.calls(),
            before + usize::from(expected == StatusCode::OK),
            "only admitted requests reach the backend"
        );
    }
}

fn mcp_request(token: Option<&str>) -> Request<Body> {
    let mut request = Request::post("/cua/mcp")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(CALL))
        .unwrap();
    if let Some(token) = token {
        request.headers_mut().insert(
            header::AUTHORIZATION,
            HeaderValue::from_str(&format!("Bearer {token}")).unwrap(),
        );
    }
    request
}

async fn send(router: &Router, request: Request<Body>) -> Response {
    tokio::time::timeout(WAIT, router.clone().oneshot(request))
        .await
        .expect("router request timed out")
        .unwrap()
}

async fn init(router: &Router, body: Value, expected: StatusCode) {
    let response = send(
        router,
        Request::post("/init")
            .body(Body::from(body.to_string()))
            .unwrap(),
    )
    .await;
    assert_eq!(response.status(), expected, "init {body}");
}

async fn get(router: &Router, path: &str) -> Response {
    send(router, Request::get(path).body(Body::empty()).unwrap()).await
}

async fn text(response: Response) -> String {
    String::from_utf8(
        to_bytes(response.into_body(), 64 << 10)
            .await
            .unwrap()
            .to_vec(),
    )
    .unwrap()
}

async fn document(router: &Router, path: &str) -> (StatusCode, Value) {
    let response = get(router, path).await;
    let status = response.status();
    (status, serde_json::from_str(&text(response).await).unwrap())
}

async fn metrics(router: &Router) -> String {
    let response = get(router, "/cua/metrics").await;
    assert_eq!(response.status(), StatusCode::OK);
    text(response).await
}

#[tokio::test]
async fn rejected_bearers_never_reach_backend_or_change_tool_metrics() {
    for adapter in ADAPTERS {
        let fixture = Fixture::new(adapter, false).await;
        fixture.install("first").await;
        let baseline = metrics(&fixture.router).await;
        let mut cases: Vec<Vec<HeaderValue>> = [
            vec![],
            vec![""],
            vec!["Bearer"],
            vec!["Bearer "],
            vec!["Bearer wrong"],
            vec!["Basic first"],
            vec!["Bearer  first"],
            vec!["Bearer first extra"],
            vec!["Bearer first, Bearer first"],
            vec!["Bearer first", "Bearer first"],
            vec!["Bearer first", "Bearer wrong"],
            vec!["Bearer wrong", "Bearer first"],
        ]
        .into_iter()
        .map(|values| values.into_iter().map(HeaderValue::from_static).collect())
        .collect();
        cases.push(vec![HeaderValue::from_bytes(b"Bearer \xff").unwrap()]);
        for values in cases {
            let mut request = mcp_request(None);
            // A correct S0 header must not rescue an invalid CUA credential.
            request
                .headers_mut()
                .insert("x-access-token", HeaderValue::from_static("first"));
            for value in &values {
                request
                    .headers_mut()
                    .append(header::AUTHORIZATION, value.clone());
            }
            let response = send(&fixture.router, request).await;
            assert_eq!(
                response.status(),
                StatusCode::UNAUTHORIZED,
                "{adapter:?}: {values:?}"
            );
            assert_eq!(
                response.headers().get(header::WWW_AUTHENTICATE).unwrap(),
                "Bearer"
            );
            assert_eq!(fixture.backend.calls(), 0, "{adapter:?}: {values:?}");
            assert_eq!(metrics(&fixture.router).await, baseline);
        }
        fixture.admission(Some("first"), StatusCode::OK).await;
        assert!(metrics(&fixture.router)
            .await
            .contains("coorworker_envd_cua_tool_calls_total{tool=\"probe\",status=\"ok\"} 1"));
    }
}

#[tokio::test]
async fn real_init_install_rotation_failed_stale_and_reset_are_observed_immediately() {
    for adapter in ADAPTERS {
        let fixture = Fixture::new(adapter, false).await;
        fixture.provider.set("first");
        fixture
            .admission(Some("first"), StatusCode::UNAUTHORIZED)
            .await;
        init(
            &fixture.router,
            json!({"accessToken":"first", "timestamp":"2026-11-04T00:00:00Z"}),
            StatusCode::NO_CONTENT,
        )
        .await;
        fixture.admission(Some("first"), StatusCode::OK).await;
        for body in [
            json!({"accessToken":"wrong"}),
            json!({}),
            json!({"accessToken":""}),
        ] {
            init(&fixture.router, body, StatusCode::UNAUTHORIZED).await;
            fixture.admission(Some("first"), StatusCode::OK).await;
        }
        init(
            &fixture.router,
            json!({"accessToken":42}),
            StatusCode::BAD_REQUEST,
        )
        .await;
        fixture.admission(Some("first"), StatusCode::OK).await;
        fixture.provider.set("second");
        init(
            &fixture.router,
            json!({"accessToken":"second", "timestamp":"2026-11-04T01:00:00Z"}),
            StatusCode::NO_CONTENT,
        )
        .await;
        fixture
            .admission(Some("first"), StatusCode::UNAUTHORIZED)
            .await;
        fixture.admission(Some("second"), StatusCode::OK).await;
        // MMDS authorizes this token, but a stale init must not reinstall it.
        fixture.provider.set("first");
        init(
            &fixture.router,
            json!({"accessToken":"first", "timestamp":"2026-11-04T00:00:00Z"}),
            StatusCode::NO_CONTENT,
        )
        .await;
        fixture
            .admission(Some("first"), StatusCode::UNAUTHORIZED)
            .await;
        fixture.admission(Some("second"), StatusCode::OK).await;
        fixture.provider.set("");
        init(&fixture.router, json!({}), StatusCode::NO_CONTENT).await;
        for token in [None, Some(""), Some("first"), Some("second")] {
            fixture.admission(token, StatusCode::UNAUTHORIZED).await;
        }
        // S0 preserves its existing unauthenticated behavior after reset.
        assert_eq!(get(&fixture.router, "/envs").await.status(), StatusCode::OK);
    }
}

#[tokio::test]
async fn fresh_no_token_init_and_handover_stay_closed_until_a_token_is_installed() {
    for adapter in ADAPTERS {
        for handover in [false, true] {
            let fixture = Fixture::new(adapter, handover).await;
            let baseline = metrics(&fixture.router).await;
            for token in [None, Some(""), Some("uninstalled")] {
                fixture.admission(token, StatusCode::UNAUTHORIZED).await;
            }
            fixture.provider.set("");
            init(&fixture.router, json!({}), StatusCode::NO_CONTENT).await;
            fixture.admission(None, StatusCode::UNAUTHORIZED).await;
            fixture
                .admission(Some("uninstalled"), StatusCode::UNAUTHORIZED)
                .await;
            assert_eq!(metrics(&fixture.router).await, baseline);
            fixture.install("installed").await;
            fixture.admission(Some("installed"), StatusCode::OK).await;
        }
    }
}

#[tokio::test]
async fn bearer_and_s0_access_headers_keep_their_own_boundaries() {
    for adapter in ADAPTERS {
        let fixture = Fixture::new(adapter, false).await;
        fixture.install("first").await;
        for (path, headers, expected) in [
            ("/envs", vec![], StatusCode::UNAUTHORIZED),
            (
                "/envs",
                vec![("authorization", "Bearer first")],
                StatusCode::UNAUTHORIZED,
            ),
            ("/envs", vec![("x-access-token", "first")], StatusCode::OK),
            (
                "/envs",
                vec![
                    ("x-access-token", "wrong"),
                    ("authorization", "Bearer first"),
                ],
                StatusCode::UNAUTHORIZED,
            ),
            (
                "/envs",
                vec![
                    ("x-access-token", "first"),
                    ("authorization", "Bearer wrong"),
                ],
                StatusCode::OK,
            ),
            ("/health", vec![], StatusCode::NO_CONTENT),
        ] {
            let mut request = Request::get(path);
            for (name, value) in headers {
                request = request.header(name, value);
            }
            assert_eq!(
                send(&fixture.router, request.body(Body::empty()).unwrap())
                    .await
                    .status(),
                expected,
                "{adapter:?}: {path}"
            );
        }
        let mut request = mcp_request(Some("first"));
        request
            .headers_mut()
            .insert("x-access-token", HeaderValue::from_static("wrong"));
        assert_eq!(
            send(&fixture.router, request).await.status(),
            StatusCode::OK
        );
        fixture.admission(None, StatusCode::UNAUTHORIZED).await;
    }
}

#[tokio::test]
async fn blocked_backend_releases_configuration_and_admitted_work_survives_rotation() {
    for adapter in ADAPTERS {
        let fixture = Fixture::new(adapter, false).await;
        fixture.install("first").await;
        fixture.backend.block_next.store(true, Ordering::SeqCst);
        let router = fixture.router.clone();
        let pending =
            tokio::spawn(async move { router.oneshot(mcp_request(Some("first"))).await.unwrap() });
        tokio::time::timeout(WAIT, fixture.backend.entered.notified())
            .await
            .expect("backend was not entered");
        let guard = tokio::time::timeout(WAIT, fixture.configuration.lock())
            .await
            .expect("backend held the daemon configuration lock");
        drop(guard);
        fixture.provider.set("second");
        init(
            &fixture.router,
            json!({"accessToken":"second"}),
            StatusCode::NO_CONTENT,
        )
        .await;
        fixture
            .admission(Some("first"), StatusCode::UNAUTHORIZED)
            .await;
        fixture.backend.release.notify_one();
        let response = tokio::time::timeout(WAIT, pending)
            .await
            .expect("admitted request did not finish")
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        fixture.admission(Some("second"), StatusCode::OK).await;
        assert!(metrics(&fixture.router)
            .await
            .contains("coorworker_envd_cua_tool_calls_total{tool=\"probe\",status=\"ok\"} 2"));
    }
}

#[tokio::test]
async fn shared_helper_receives_only_its_credential_and_permitted_protocol_headers() {
    let fixture = Fixture::new(Adapter::SharedExternal, false).await;
    fixture.install("daemon-secret").await;
    let mut request = mcp_request(Some("daemon-secret"));
    for (name, value) in [
        ("mcp-protocol-version", "2025-03-26"),
        ("mcp-session-id", "private-session"),
        ("x-access-token", "daemon-secret"),
        ("cookie", "session=private"),
        ("proxy-authorization", "Basic private"),
        ("x-forwarded-for", "203.0.113.1"),
        ("x-private-header", "must-not-travel"),
    ] {
        request
            .headers_mut()
            .insert(name, HeaderValue::from_static(value));
    }
    let response = send(&fixture.router, request).await;
    assert_eq!(response.status(), StatusCode::OK);
    let seen = fixture.backend.seen.lock().unwrap();
    assert_eq!(seen.len(), 1);
    let (headers, body) = &seen[0];
    assert_eq!(body.as_ref(), CALL.as_bytes());
    assert_eq!(
        headers[header::AUTHORIZATION],
        "Bearer private-helper-credential"
    );
    assert_eq!(headers["mcp-protocol-version"], "2025-03-26");
    assert_eq!(headers["mcp-session-id"], "private-session");
    for name in [
        "x-access-token",
        "cookie",
        "proxy-authorization",
        "x-forwarded-for",
        "x-private-header",
    ] {
        assert!(!headers.contains_key(name), "forwarded {name}");
    }
    assert!(headers.values().all(|value| !value
        .as_bytes()
        .windows(b"daemon-secret".len())
        .any(|part| part == b"daemon-secret")));
}

#[tokio::test]
async fn shared_helper_requires_a_nonempty_transport_credential() {
    let (address, _server) = serve(Arc::new(Backend::default()).router()).await;
    let configuration = Arc::new(AsyncMutex::new(Configuration::new()));
    assert!(
        CuaExtension::adopt_shared(address, configuration, Vec::new())
            .await
            .is_err()
    );
}

#[tokio::test]
async fn embedded_discovery_has_no_fictitious_helper_or_worker_lifecycle() {
    let fixture = Fixture::new(Adapter::Embedded, false).await;
    let (status, info) = document(&fixture.router, "/cua/info").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(info["proxy_path"], "/cua/mcp");
    assert_eq!(info["proxy_auth"], "bearer");
    assert!(info.get("mcp_url").is_none());
    assert!(!info.to_string().contains("http://"));
    let (_, health) = document(&fixture.router, "/cua/health").await;
    for key in ["restarts", "observed_restarts", "worker_up"] {
        if let Some(value) = health.get(key) {
            assert!(
                value == 0 || value == false,
                "embedded health invents {key}: {value}"
            );
        }
    }
    if let Some(value) = health.get("budget_exhausted") {
        assert_eq!(value, false);
    }
    // Legacy worker series may remain for compatibility, but an embedded
    // router cannot claim a live helper or any worker restart.
    for line in metrics(&fixture.router)
        .await
        .lines()
        .filter(|line| line.starts_with("coorworker_envd_cua_worker_"))
    {
        let value: f64 = line.rsplit_once(' ').unwrap().1.parse().unwrap();
        assert_eq!(value, 0.0, "embedded worker metric: {line}");
    }
}

#[tokio::test]
async fn health_separates_available_runtime_from_live_authentication_readiness() {
    for adapter in ADAPTERS {
        let fixture = Fixture::new(adapter, false).await;
        for token in [None, Some("first"), None] {
            match token {
                Some(token) => fixture.install(token).await,
                None => {
                    fixture.provider.set("");
                    init(&fixture.router, json!({}), StatusCode::NO_CONTENT).await;
                }
            }
            let (status, health) = document(&fixture.router, "/cua/health").await;
            assert_eq!(health["runtime_available"], true, "{adapter:?}: {health}");
            assert_eq!(health["ready"], token.is_some(), "{adapter:?}: {health}");
            assert_eq!(
                health["auth_ready"],
                token.is_some(),
                "{adapter:?}: {health}"
            );
            assert_eq!(
                status,
                if token.is_some() {
                    StatusCode::OK
                } else {
                    StatusCode::SERVICE_UNAVAILABLE
                }
            );
            let (_, info) = document(&fixture.router, "/cua/info").await;
            assert_eq!(info["auth_source"], "envd");
            assert_eq!(info["auth_configured"], token.is_some());
            assert_eq!(info["proxy_path"], "/cua/mcp");
            assert_eq!(
                info["mode"],
                match adapter {
                    Adapter::Embedded => "embedded",
                    Adapter::SharedExternal => "shared-external",
                }
            );
            for path in ["/cua/info", "/cua/health"] {
                let (_, document) = document(&fixture.router, path).await;
                assert!(!document.to_string().contains("private-helper-credential"));
                assert!(!document.to_string().contains("first"));
            }
        }
    }
}
