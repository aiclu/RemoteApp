use std::{fmt, net::Ipv6Addr, sync::Arc};

use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;
use zeroize::{Zeroize, ZeroizeOnDrop};

pub const DEFAULT_RDP_PORT: u16 = 3389;
pub const DEFAULT_DESKTOP_WIDTH: u16 = 1920;
pub const DEFAULT_DESKTOP_HEIGHT: u16 = 1080;
pub const DEFAULT_DESKTOP_SCALE_FACTOR: u32 = 100;
pub const MAX_FRAME_BYTES: usize = 256 * 1024 * 1024;
pub const MAX_DESKTOP_DIMENSION: u32 = 8192;

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
pub struct ProfileId(Uuid);

impl ProfileId {
    #[must_use]
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }

    #[must_use]
    pub const fn from_uuid(value: Uuid) -> Self {
        Self(value)
    }

    #[must_use]
    pub const fn as_uuid(self) -> Uuid {
        self.0
    }
}

impl Default for ProfileId {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for ProfileId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct ConnectionProfile {
    pub id: ProfileId,
    pub label: String,
    pub host: String,
    pub port: u16,
    pub username: String,
    pub domain: Option<String>,
    pub desktop: DesktopConfig,
    pub scale_mode: ScaleMode,
    pub certificate_policy: CertificatePolicy,
}

impl Default for ConnectionProfile {
    fn default() -> Self {
        Self {
            id: ProfileId::new(),
            label: String::new(),
            host: String::new(),
            port: DEFAULT_RDP_PORT,
            username: String::new(),
            domain: None,
            desktop: DesktopConfig::default(),
            scale_mode: ScaleMode::Fit,
            certificate_policy: CertificatePolicy::TrustOnFirstUse { fingerprint: None },
        }
    }
}

impl ConnectionProfile {
    pub fn validate(&self) -> Result<(), ProfileValidationError> {
        let host = self.host.trim();
        if host.is_empty() {
            return Err(ProfileValidationError::MissingHost);
        }
        if host.len() > 255 || host.chars().any(char::is_whitespace) {
            return Err(ProfileValidationError::InvalidHost);
        }
        if self.port == 0 {
            return Err(ProfileValidationError::InvalidPort);
        }
        if self.username.trim().is_empty() {
            return Err(ProfileValidationError::MissingUsername);
        }
        self.desktop.validate()?;
        self.certificate_policy.validate()?;
        Ok(())
    }

    #[must_use]
    pub fn endpoint(&self) -> String {
        if self.host.contains(':') && !self.host.starts_with('[') {
            format!("[{}]:{}", self.host, self.port)
        } else {
            format!("{}:{}", self.host, self.port)
        }
    }
}

#[derive(Debug, Error, Eq, PartialEq)]
pub enum EndpointParseError {
    #[error("RDP endpoint is missing")]
    MissingHost,
    #[error("RDP endpoint has an invalid host")]
    InvalidHost,
    #[error("RDP endpoint has an invalid port")]
    InvalidPort,
}

pub fn parse_endpoint(input: &str) -> Result<(String, u16), EndpointParseError> {
    let value = input.trim();
    if value.is_empty() {
        return Err(EndpointParseError::MissingHost);
    }

    if let Some(value) = value.strip_prefix('[') {
        let closing = value.find(']').ok_or(EndpointParseError::InvalidHost)?;
        let host = &value[..closing];
        validate_endpoint_host(host)?;
        let suffix = &value[closing + 1..];
        let port = if suffix.is_empty() {
            DEFAULT_RDP_PORT
        } else if let Some(raw_port) = suffix.strip_prefix(':') {
            parse_endpoint_port(raw_port)?
        } else {
            return Err(EndpointParseError::InvalidHost);
        };
        return Ok((host.to_owned(), port));
    }

    if value.matches(':').count() == 1 {
        let (host, raw_port) = value
            .split_once(':')
            .expect("a single colon always has two split parts");
        validate_endpoint_host(host)?;
        return Ok((host.to_owned(), parse_endpoint_port(raw_port)?));
    }

    validate_endpoint_host(value)?;
    Ok((value.to_owned(), DEFAULT_RDP_PORT))
}

fn parse_endpoint_port(value: &str) -> Result<u16, EndpointParseError> {
    let port = value
        .parse::<u16>()
        .map_err(|_| EndpointParseError::InvalidPort)?;
    (port != 0)
        .then_some(port)
        .ok_or(EndpointParseError::InvalidPort)
}

fn validate_endpoint_host(host: &str) -> Result<(), EndpointParseError> {
    if host.is_empty() {
        return Err(EndpointParseError::MissingHost);
    }
    if host.len() > 255
        || host.chars().any(char::is_whitespace)
        || host
            .chars()
            .any(|character| matches!(character, '/' | '\\'))
    {
        return Err(EndpointParseError::InvalidHost);
    }
    if host.contains(':') && host.parse::<Ipv6Addr>().is_err() {
        return Err(EndpointParseError::InvalidHost);
    }
    Ok(())
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct DesktopConfig {
    pub width: u16,
    pub height: u16,
    pub scale_factor: u32,
}

impl Default for DesktopConfig {
    fn default() -> Self {
        Self {
            width: DEFAULT_DESKTOP_WIDTH,
            height: DEFAULT_DESKTOP_HEIGHT,
            scale_factor: DEFAULT_DESKTOP_SCALE_FACTOR,
        }
    }
}

impl DesktopConfig {
    pub fn validate(&self) -> Result<(), ProfileValidationError> {
        if self.width < 320 || self.height < 200 {
            return Err(ProfileValidationError::DesktopTooSmall);
        }
        if u32::from(self.width) > MAX_DESKTOP_DIMENSION
            || u32::from(self.height) > MAX_DESKTOP_DIMENSION
        {
            return Err(ProfileValidationError::DesktopTooLarge);
        }
        if !(50..=500).contains(&self.scale_factor) {
            return Err(ProfileValidationError::InvalidScaleFactor);
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum ScaleMode {
    Fit,
    Fill,
    OneToOne,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum CertificatePolicy {
    Strict,
    TrustOnFirstUse { fingerprint: Option<String> },
}

impl CertificatePolicy {
    pub fn validate(&self) -> Result<(), ProfileValidationError> {
        if let Self::TrustOnFirstUse {
            fingerprint: Some(value),
        } = self
        {
            let normalized = value.replace(':', "");
            if normalized.len() != 64
                || !normalized
                    .chars()
                    .all(|character| character.is_ascii_hexdigit())
            {
                return Err(ProfileValidationError::InvalidCertificateFingerprint);
            }
        }
        Ok(())
    }

    #[must_use]
    pub fn fingerprint(&self) -> Option<&str> {
        match self {
            Self::Strict => None,
            Self::TrustOnFirstUse { fingerprint } => fingerprint.as_deref(),
        }
    }
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum ProfileValidationError {
    #[error("RDP host is missing")]
    MissingHost,
    #[error("RDP host is invalid")]
    InvalidHost,
    #[error("RDP port must be between 1 and 65535")]
    InvalidPort,
    #[error("RDP username is missing")]
    MissingUsername,
    #[error("desktop size is too small")]
    DesktopTooSmall,
    #[error("desktop size is too large")]
    DesktopTooLarge,
    #[error("desktop scale factor must be between 50 and 500")]
    InvalidScaleFactor,
    #[error("certificate fingerprint must be a SHA-256 hex value")]
    InvalidCertificateFingerprint,
}

#[derive(Clone, Zeroize, ZeroizeOnDrop)]
pub struct Secret(String);

impl Secret {
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    #[must_use]
    pub fn expose(&self) -> &str {
        &self.0
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("<redacted>")
    }
}

impl From<String> for Secret {
    fn from(value: String) -> Self {
        Self::new(value)
    }
}

impl From<&str> for Secret {
    fn from(value: &str) -> Self {
        Self::new(value)
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Rect {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

impl Rect {
    #[must_use]
    pub const fn full(width: u32, height: u32) -> Self {
        Self {
            x: 0,
            y: 0,
            width,
            height,
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum PixelFormat {
    Rgba8888,
    Bgra8888,
}

#[derive(Clone, Debug)]
pub struct FrameBuffer(Arc<[u8]>);

impl FrameBuffer {
    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.0.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

#[derive(Clone, Debug)]
pub struct FrameUpdate {
    pub sequence: u64,
    pub width: u32,
    pub height: u32,
    pub pixel_format: PixelFormat,
    pub damage_rects: Vec<Rect>,
    pub buffer: FrameBuffer,
}

impl FrameUpdate {
    pub fn new(
        sequence: u64,
        width: u32,
        height: u32,
        pixel_format: PixelFormat,
        data: Vec<u8>,
        damage_rects: Vec<Rect>,
    ) -> Result<Self, FrameError> {
        let pixels = width
            .checked_mul(height)
            .ok_or(FrameError::DimensionOverflow)?;
        let expected_bytes = usize::try_from(pixels)
            .ok()
            .and_then(|value| value.checked_mul(4))
            .ok_or(FrameError::DimensionOverflow)?;
        if expected_bytes != data.len() {
            return Err(FrameError::InvalidBufferLength {
                expected: expected_bytes,
                actual: data.len(),
            });
        }
        if data.len() > MAX_FRAME_BYTES {
            return Err(FrameError::TooLarge);
        }
        if width == 0
            || height == 0
            || width > MAX_DESKTOP_DIMENSION
            || height > MAX_DESKTOP_DIMENSION
        {
            return Err(FrameError::InvalidDimensions);
        }
        Ok(Self {
            sequence,
            width,
            height,
            pixel_format,
            damage_rects,
            buffer: FrameBuffer(Arc::from(data)),
        })
    }
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum FrameError {
    #[error("frame dimensions overflow")]
    DimensionOverflow,
    #[error("frame dimensions are invalid")]
    InvalidDimensions,
    #[error("frame buffer length mismatch: expected {expected}, got {actual}")]
    InvalidBufferLength { expected: usize, actual: usize },
    #[error("frame exceeds the memory limit")]
    TooLarge,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profile_validation_accepts_ipv4_and_ipv6_endpoints() {
        let mut profile = ConnectionProfile {
            host: "192.0.2.10".into(),
            username: "alice".into(),
            ..Default::default()
        };
        assert!(profile.validate().is_ok());
        assert_eq!(profile.endpoint(), "192.0.2.10:3389");

        profile.host = "2001:db8::10".into();
        assert_eq!(profile.endpoint(), "[2001:db8::10]:3389");
    }

    #[test]
    fn endpoint_parser_accepts_custom_port() {
        assert_eq!(
            parse_endpoint("192.0.2.10:45988"),
            Ok(("192.0.2.10".into(), 45988))
        );
        assert_eq!(
            parse_endpoint(" server.example:3389 "),
            Ok(("server.example".into(), 3389))
        );
    }

    #[test]
    fn endpoint_parser_accepts_bracketed_ipv6() {
        assert_eq!(
            parse_endpoint("[2001:db8::10]:3390"),
            Ok(("2001:db8::10".into(), 3390))
        );
        assert_eq!(
            parse_endpoint("2001:db8::10"),
            Ok(("2001:db8::10".into(), DEFAULT_RDP_PORT))
        );
    }

    #[test]
    fn endpoint_parser_rejects_invalid_port() {
        assert_eq!(
            parse_endpoint("192.0.2.10:65536"),
            Err(EndpointParseError::InvalidPort)
        );
        assert_eq!(
            parse_endpoint("192.0.2.10:0"),
            Err(EndpointParseError::InvalidPort)
        );
    }

    #[test]
    fn profile_validation_rejects_invalid_fingerprint() {
        let profile = ConnectionProfile {
            host: "server".into(),
            username: "alice".into(),
            certificate_policy: CertificatePolicy::TrustOnFirstUse {
                fingerprint: Some("not-a-fingerprint".into()),
            },
            ..Default::default()
        };
        assert_eq!(
            profile.validate(),
            Err(ProfileValidationError::InvalidCertificateFingerprint)
        );
    }

    #[test]
    fn frame_rejects_wrong_buffer_length() {
        let error = FrameUpdate::new(1, 2, 2, PixelFormat::Rgba8888, vec![0; 3], vec![])
            .expect_err("invalid buffer length should fail");
        assert_eq!(
            error,
            FrameError::InvalidBufferLength {
                expected: 16,
                actual: 3
            }
        );
    }
}
