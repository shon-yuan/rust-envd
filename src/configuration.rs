//! The per-daemon mutable state every HTTP adapter shares.
//!
//! The Linux daemon and the Windows daemon both hold an
//! `Arc<Mutex<Configuration>>`; auth, `/envs` and the `/files` handlers read
//! from it without knowing which platform they run on.
use crate::auth::Access;
use crate::exec_context::{Defaults, Environment};

pub struct Configuration {
    pub access: Access,
    pub defaults: Defaults,
    pub environment: Environment,
}

impl Configuration {
    /// A boot configuration: fresh auth, builtin defaults, and the
    /// `E2B_SANDBOX=true` internal marker both daemons start with.
    pub fn new() -> Self {
        let mut environment = Environment::default();
        environment.store_internal("E2B_SANDBOX", "true");
        Self {
            access: Access::fresh(),
            defaults: Defaults::default(),
            environment,
        }
    }
}

impl Default for Configuration {
    fn default() -> Self {
        Self::new()
    }
}
