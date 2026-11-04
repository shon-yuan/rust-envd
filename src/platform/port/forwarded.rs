//! The live-upgrade forward record and the guest gateway address.
//!
//! `ForwardedPort` is one active port-forward carried across a live upgrade
//! (handover.proto `ForwardedPort` fields 1-5). The wire encoding is the
//! generated `proto::upgrade::ForwardedPort` — byte-identical to Go's
//! `proto.Marshal` by construction — and `to_wire`/`from_wire` are the
//! conversions the upgrade blob travels through. The type itself is
//! platform-neutral because the Linux daemon encodes it and the Windows
//! daemon (which carries no forwards, see `docs/windows-port.md` §8.3) keeps
//! the same record shape.

/// Oracle forward.go:31 — the guest gateway IP the forwards bind.
pub const DEFAULT_GATEWAY_IP: [u8; 4] = [169, 254, 0, 21];

/// One active port-forward carried across a live upgrade.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ForwardedPort {
    /// The forwarder map key: "<listener_pid>-<port>" (forward.go:160).
    pub key: String,
    pub port: u32,
    /// pid of the guest process listening on the port.
    pub listener_pid: i32,
    /// IP version (4 or 6).
    pub family: u32,
    /// pid of the running forwarder child to re-adopt (0 on Windows, which
    /// forwards in-process and therefore carries nothing across a handover).
    pub socat_pid: i32,
}

impl ForwardedPort {
    pub fn to_wire(&self) -> crate::proto::upgrade::ForwardedPort {
        crate::proto::upgrade::ForwardedPort {
            key: self.key.clone(),
            port: self.port,
            listener_pid: self.listener_pid,
            family: self.family,
            socat_pid: self.socat_pid,
            ..Default::default()
        }
    }

    pub fn from_wire(port: &crate::proto::upgrade::ForwardedPort) -> Self {
        Self {
            key: port.key.clone(),
            port: port.port,
            listener_pid: port.listener_pid,
            family: port.family,
            socat_pid: port.socat_pid,
        }
    }
}

/// Oracle forward.go:24-29; shared because both forwarders run the same
/// refresh state machine.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PortState {
    Forward,
    Delete,
}
