//! Platform-neutral domain and session boundaries for RemoteAPP.
//!
//! The Android UI only sees the types in this crate. The IronRDP dependency is kept behind the
//! `ironrdp` feature so the domain model remains usable by desktop tools and deterministic tests.

mod input;
mod model;
mod session;

#[cfg(feature = "ironrdp")]
mod ironrdp_backend;

pub use input::{InputQueue, KeyCode, MouseButton, TouchpadMapper};
pub use model::{
    CertificatePolicy, ConnectionProfile, DesktopConfig, EndpointParseError, FrameBuffer,
    FrameUpdate, PixelFormat, ProfileId, Rect, ScaleMode, Secret, parse_endpoint,
};
pub use session::{
    DisconnectReason, ReconnectPolicy, SessionCommand, SessionError, SessionEvent, SessionHandle,
    SessionStart, SessionState, spawn_session,
};

#[cfg(feature = "ironrdp")]
pub use ironrdp_backend::IronRdpBackend;
