//! Extension surfaces layered on top of the frozen envd protocol surface.
//!
//! The contract this module enforces is what makes "superset" safe:
//!
//! * **S0 is frozen.** The official envd surface (the routes in
//!   [`crate::daemon::http_routes`] and the Connect fallback) is reproduced
//!   exactly and is not touched by an extension. An extension can never
//!   shadow an S0 path: it is mounted with [`axum::Router::nest`] under
//!   `/<name>`, and [`Extensions::new`] refuses a name that is reserved by an
//!   S0 first path segment. Zero extensions therefore leave the router
//!   byte-for-byte unchanged.
//! * **Layering is separated.** Extensions are merged *after* the envd
//!   layers ([`crate::http::layered`]) are applied, so the envd frontdoor,
//!   authorization and CORS middleware never run for an extension request.
//!   An extension owns its own authentication, status codes and framing;
//!   reusing the `/init` access token for an extension is a defect, not a
//!   convenience.
//! * **Evidence is separated.** The differential gates compare S0 only. An
//!   extension is verified by its own contract tests and, where an upstream
//!   client exists, by that client's conformance run. A green extension run
//!   is never evidence for an S0 claim, and vice versa.
//!
//! The live extension registered by this crate is `cua`; the seam is
//! deliberately small so a second one costs a module, not a refactor.

use std::collections::BTreeSet;
use std::sync::Arc;

use axum::Router;

pub mod cua;

/// Longest accepted extension name. The bound keeps the mounted prefix a
/// single, readable path segment.
const MAX_NAME_LEN: usize = 63;

/// First path segments owned by S0. An extension may not use one of these as
/// its name: `nest` would then place its routes under a prefix that already
/// belongs to the envd surface.
const RESERVED: &[&str] = &[
    "health", "metrics", "envs", "init", "upgrade", "freeze", "unfreeze", "collapse", "fsfreeze",
    "fsthaw", "files",
];

/// Why a set of extensions could not be assembled.
///
/// These are startup failures: a daemon that cannot mount the surface it was
/// asked to serve must say so before binding the listener, not answer 404 on
/// the first call.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ExtensionError {
    #[error("invalid extension name {0:?}: expected [a-z][a-z0-9_-]{{0,62}}")]
    InvalidName(String),
    #[error("extension name {0:?} is reserved by the envd surface")]
    Reserved(String),
    #[error("duplicate extension name {0:?}")]
    Duplicate(String),
}

/// One additive surface.
///
/// The router returned by [`Extension::router`] carries paths relative to the
/// extension's own prefix (`/health` becomes `/<name>/health`). It must not
/// have a fallback: the envd router owns the process-wide 404 shape.
pub trait Extension: Send + Sync {
    /// The path segment this surface is mounted under.
    fn name(&self) -> &str;

    /// The surface itself, with its own layers already applied.
    fn router(&self) -> Router;

    /// Readiness of this surface. Defaults to healthy; an extension whose
    /// backend is unavailable reports it here rather than by failing S0.
    fn health(&self) -> Health {
        Health::healthy()
    }
}

/// Aggregate readiness of one extension.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Health {
    pub state: HealthState,
    /// Human-readable reason, empty while healthy.
    pub detail: String,
}

impl Health {
    pub fn healthy() -> Self {
        Self {
            state: HealthState::Healthy,
            detail: String::new(),
        }
    }

    pub fn degraded(detail: impl Into<String>) -> Self {
        Self {
            state: HealthState::Degraded,
            detail: detail.into(),
        }
    }

    pub fn unhealthy(detail: impl Into<String>) -> Self {
        Self {
            state: HealthState::Unhealthy,
            detail: detail.into(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HealthState {
    Healthy,
    Degraded,
    Unhealthy,
}

/// A validated, ordered set of extension surfaces.
///
/// Built once at startup. [`Extensions::empty`] is the parity build: mounting
/// it is a no-op.
#[derive(Clone, Default)]
pub struct Extensions {
    items: Vec<Arc<dyn Extension>>,
}

impl std::fmt::Debug for Extensions {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Extensions")
            .field("names", &self.names().collect::<Vec<_>>())
            .finish()
    }
}

impl Extensions {
    /// No extensions: the router is left exactly as the envd surface built it.
    pub fn empty() -> Self {
        Self::default()
    }

    /// Validates every name before anything is mounted.
    pub fn new(items: Vec<Arc<dyn Extension>>) -> Result<Self, ExtensionError> {
        let mut names = BTreeSet::new();
        for item in &items {
            validate_name(item.name())?;
            if !names.insert(item.name().to_owned()) {
                return Err(ExtensionError::Duplicate(item.name().to_owned()));
            }
        }
        Ok(Self { items })
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.items.iter().map(|item| item.name())
    }

    /// Merges every surface under its prefix.
    ///
    /// Call this on the router returned by [`crate::http::layered`], not
    /// before it: merging after the layers keeps the envd middleware off the
    /// extension surface.
    pub fn mount(&self, router: Router) -> Router {
        self.items.iter().fold(router, |router, item| {
            let prefix = format!("/{}", item.name());
            router.merge(Router::new().nest(&prefix, item.router()))
        })
    }

    /// Readiness for each `(name, health)` pair, in declaration order.
    pub fn health(&self) -> Vec<(&str, Health)> {
        self.items
            .iter()
            .map(|item| (item.name(), item.health()))
            .collect()
    }
}

fn validate_name(name: &str) -> Result<(), ExtensionError> {
    let valid = !name.is_empty()
        && name.len() <= MAX_NAME_LEN
        && name
            .chars()
            .next()
            .is_some_and(|first| first.is_ascii_lowercase())
        && name.chars().all(|character| {
            character.is_ascii_lowercase()
                || character.is_ascii_digit()
                || character == '-'
                || character == '_'
        });
    if !valid {
        return Err(ExtensionError::InvalidName(name.to_owned()));
    }
    if RESERVED.contains(&name) {
        return Err(ExtensionError::Reserved(name.to_owned()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::StatusCode;
    use axum::routing::get;
    use tower::ServiceExt;

    struct Surface(&'static str);

    impl Extension for Surface {
        fn name(&self) -> &str {
            self.0
        }

        fn router(&self) -> Router {
            Router::new().route("/ping", get(|| async { "pong" }))
        }
    }

    fn surface(name: &'static str) -> Arc<dyn Extension> {
        Arc::new(Surface(name))
    }

    #[test]
    fn accepts_a_namespaced_name() {
        let extensions = Extensions::new(vec![surface("cua")]).unwrap();
        assert_eq!(extensions.names().collect::<Vec<_>>(), ["cua"]);
        assert!(!extensions.is_empty());
    }

    #[test]
    fn rejects_names_that_cannot_be_a_path_segment() {
        for name in ["", "Cua", "9cua", "cua/v1", "cua.v1", "cua v1", "-cua"] {
            assert_eq!(
                Extensions::new(vec![surface(name)]).unwrap_err(),
                ExtensionError::InvalidName(name.to_owned()),
                "{name:?}"
            );
        }
    }

    #[test]
    fn rejects_names_owned_by_the_envd_surface() {
        for name in ["health", "files", "init"] {
            assert_eq!(
                Extensions::new(vec![surface(name)]).unwrap_err(),
                ExtensionError::Reserved(name.to_owned()),
                "{name:?}"
            );
        }
    }

    #[test]
    fn rejects_duplicates() {
        assert_eq!(
            Extensions::new(vec![surface("cua"), surface("cua")]).unwrap_err(),
            ExtensionError::Duplicate("cua".to_owned())
        );
    }

    #[tokio::test]
    async fn an_empty_set_leaves_the_router_alone() {
        let router = Router::new().route("/health", get(|| async { "envd" }));
        let mounted = Extensions::empty().mount(router);
        let response = mounted
            .oneshot(
                axum::http::Request::get("/health")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn mounts_under_the_prefix_only() {
        let extensions = Extensions::new(vec![surface("cua")]).unwrap();
        let mounted = extensions
            .mount(Router::new().route("/health", get(|| async { StatusCode::NO_CONTENT })));

        let prefixed = mounted
            .clone()
            .oneshot(
                axum::http::Request::get("/cua/ping")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(prefixed.status(), StatusCode::OK);

        let bare = mounted
            .oneshot(
                axum::http::Request::get("/ping")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(bare.status(), StatusCode::NOT_FOUND);
    }

    #[test]
    fn health_defaults_to_healthy() {
        let extensions = Extensions::new(vec![surface("cua")]).unwrap();
        assert_eq!(
            extensions.health(),
            vec![("cua", Health::healthy())],
            "an extension that reports nothing is healthy"
        );
    }
}
