use std::{sync::Arc, time::Duration};

use thiserror::Error;
use tokio::sync::mpsc;

use super::input::{InputOperation, KeyCode, MouseButton};
use super::model::{ConnectionProfile, FrameUpdate, Secret};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SessionState {
    Idle,
    Connecting,
    Connected,
    Reconnecting,
    Disconnecting,
    Disconnected,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ReconnectPolicy {
    pub max_attempts: u32,
    pub base_delay: Duration,
    pub max_delay: Duration,
}

impl Default for ReconnectPolicy {
    fn default() -> Self {
        Self {
            max_attempts: 6,
            base_delay: Duration::from_secs(1),
            max_delay: Duration::from_secs(30),
        }
    }
}

impl ReconnectPolicy {
    #[must_use]
    pub fn delay_for(self, attempt: u32) -> Duration {
        let exponent = attempt.saturating_sub(1).min(5);
        let multiplier = 1_u32 << exponent;
        (self.base_delay * multiplier).min(self.max_delay)
    }
}

#[derive(Debug)]
pub struct SessionStart {
    pub profile: ConnectionProfile,
    pub password: Secret,
    pub reconnect: ReconnectPolicy,
}

#[derive(Debug, Clone)]
pub enum SessionCommand {
    Input(InputOperation),
    PointerMove {
        x: u16,
        y: u16,
    },
    ButtonDown(MouseButton),
    ButtonUp(MouseButton),
    Wheel {
        vertical: bool,
        units: i16,
    },
    KeyDown(KeyCode),
    KeyUp(KeyCode),
    SetLocalClipboard(String),
    RequestRemoteClipboard,
    Resize {
        width: u16,
        height: u16,
        scale_factor: u32,
    },
    TrustCertificate {
        fingerprint: String,
    },
    SuspendRendering,
    ResumeRendering,
    Disconnect,
}

#[derive(Debug)]
pub enum SessionEvent {
    StateChanged(SessionState),
    Connected { width: u32, height: u32 },
    Frame(Arc<FrameUpdate>),
    ClipboardText(String),
    CertificateTrustRequired { fingerprint: String },
    Reconnecting { attempt: u32, maximum_attempts: u32 },
    Disconnected { reason: DisconnectReason },
    Error(SessionError),
}

#[derive(Clone, Debug, Error)]
pub enum SessionError {
    #[error("invalid connection profile: {0}")]
    InvalidProfile(String),
    #[error("RDP backend is not available in this build")]
    BackendUnavailable,
    #[error("RDP client failed: {0}")]
    Backend(String),
    #[error("session channel closed")]
    ChannelClosed,
    #[error("certificate rejected")]
    CertificateRejected,
    #[error("authentication failed")]
    AuthenticationFailed,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DisconnectReason {
    UserRequested,
    AuthenticationFailed,
    CertificateRejected,
    TransportLost,
    ProtocolError,
    Backend(String),
}

pub struct SessionHandle {
    pub commands: mpsc::UnboundedSender<SessionCommand>,
    pub events: mpsc::Receiver<SessionEvent>,
}

pub trait SessionBackend: Send + Sync + 'static {
    fn spawn(&self, start: SessionStart) -> Result<SessionHandle, SessionError>;
}

pub fn spawn_session(start: SessionStart) -> Result<SessionHandle, SessionError> {
    #[cfg(feature = "ironrdp")]
    {
        return super::ironrdp_backend::IronRdpBackend.spawn(start);
    }
    #[cfg(not(feature = "ironrdp"))]
    {
        let _ = start;
        Err(SessionError::BackendUnavailable)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{CertificatePolicy, ConnectionProfile};

    #[test]
    fn reconnect_delay_is_bounded() {
        let policy = ReconnectPolicy::default();
        assert_eq!(policy.delay_for(1), Duration::from_secs(1));
        assert_eq!(policy.delay_for(5), Duration::from_secs(16));
        assert_eq!(policy.delay_for(9), Duration::from_secs(30));
    }

    #[test]
    fn session_start_debug_does_not_expose_password() {
        let start = SessionStart {
            profile: ConnectionProfile {
                host: "server".into(),
                username: "alice".into(),
                certificate_policy: CertificatePolicy::Strict,
                ..Default::default()
            },
            password: Secret::new("do-not-print"),
            reconnect: ReconnectPolicy::default(),
        };
        assert!(!format!("{start:?}").contains("do-not-print"));
    }
}
