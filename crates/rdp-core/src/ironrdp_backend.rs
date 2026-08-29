use std::{
    sync::{Arc, Mutex},
    thread,
};

use sha2::{Digest, Sha256};
use tokio::sync::mpsc;
use tracing::{debug, warn};

use ironrdp_client::config::{ClipboardType, ConfigBuilder, Destination, TransportKind};
use ironrdp_client::rdp::{
    AutoReconnectDecision, RdpClient, RdpInputEvent, RdpInputSender, RdpOutputEvent,
};
use ironrdp_cliprdr::backend::{ClipboardMessage, CliprdrBackend, CliprdrBackendFactory};
use ironrdp_cliprdr::pdu::{
    ClipboardFormat, ClipboardFormatId, ClipboardGeneralCapabilityFlags, FormatDataRequest,
    FormatDataResponse,
};
use ironrdp_core::{IntoOwned as _, impl_as_any};
use ironrdp_input::{
    Database as InputDatabase, MouseButton as IronMouseButton, MousePosition, Operation, Scancode,
    WheelRotations,
};
use ironrdp_pdu::{gcc::Monitor, rdp::capability_sets::MajorPlatformType};

use crate::input::{InputOperation, KeyCode, MouseButton};
use crate::model::{CertificatePolicy, ConnectionProfile, FrameUpdate, PixelFormat, Rect, Secret};
use crate::session::{
    DisconnectReason, SessionBackend, SessionCommand, SessionError, SessionEvent, SessionHandle,
    SessionStart, SessionState,
};

const OUTPUT_QUEUE_CAPACITY: usize = 4;
const RDP_CLIENT_BUILD: u32 = 1;
const RDP_CLIENT_DIR: &str = "/";
const RDP_CLIENT_NAME: &str = "RemoteAPP";

#[derive(Debug, Default, Clone, Copy)]
pub struct IronRdpBackend;

impl SessionBackend for IronRdpBackend {
    fn spawn(&self, start: SessionStart) -> Result<SessionHandle, SessionError> {
        start
            .profile
            .validate()
            .map_err(|error| SessionError::InvalidProfile(error.to_string()))?;

        let (command_sender, command_receiver) = mpsc::unbounded_channel();
        let (event_sender, event_receiver) = mpsc::channel(OUTPUT_QUEUE_CAPACITY);
        let handle = SessionHandle {
            commands: command_sender,
            events: event_receiver,
        };

        thread::Builder::new()
            .name("remoteapp-rdp".to_owned())
            .spawn(move || {
                let runtime = match tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                {
                    Ok(runtime) => runtime,
                    Err(error) => {
                        let _ = event_sender.blocking_send(SessionEvent::Error(
                            SessionError::Backend(format!("failed to create RDP runtime: {error}")),
                        ));
                        return;
                    }
                };
                runtime.block_on(run_session(start, command_receiver, event_sender));
            })
            .map_err(|error| {
                SessionError::Backend(format!("failed to spawn RDP thread: {error}"))
            })?;

        Ok(handle)
    }
}

async fn run_session(
    start: SessionStart,
    mut commands: mpsc::UnboundedReceiver<SessionCommand>,
    events: mpsc::Sender<SessionEvent>,
) {
    let _ = events
        .send(SessionEvent::StateChanged(SessionState::Connecting))
        .await;
    let clipboard_text = Arc::new(Mutex::new(String::new()));
    let (clipboard_messages, mut clipboard_receiver) =
        mpsc::unbounded_channel::<ClipboardMessage>();
    let (output_sender, mut output_receiver) = mpsc::channel(OUTPUT_QUEUE_CAPACITY);
    let (config, clipboard_factory) = match build_config(
        &start.profile,
        &start.password,
        events.clone(),
        clipboard_text.clone(),
        clipboard_messages,
    ) {
        Ok(config) => config,
        Err(error) => {
            let _ = events.send(SessionEvent::Error(error.clone())).await;
            let _ = events
                .send(SessionEvent::Disconnected {
                    reason: DisconnectReason::Backend(error.to_string()),
                })
                .await;
            return;
        }
    };

    let client = RdpClient::new(config, output_sender)
        .with_cliprdr_backend_factory(Box::new(clipboard_factory))
        .with_auto_reconnect(start.reconnect.max_attempts);
    let input_sender = client.input_sender();
    let mut rdp_task = Box::pin(client.run());
    let mut input_database = InputDatabase::new();
    let mut rendering_suspended = false;
    let mut last_frame_sequence = 0_u64;
    let mut command_closed = false;
    let mut rdp_finished = false;
    let mut rdp_task_finished = false;
    let mut user_disconnect_requested = false;
    let mut terminal_reason = None;

    while !rdp_finished {
        tokio::select! {
            maybe_command = commands.recv(), if !command_closed => {
                match maybe_command {
                    Some(command) => {
                        if matches!(&command, SessionCommand::Disconnect) {
                            user_disconnect_requested = true;
                        }
                        if let Err(error) = dispatch_command(
                            command,
                            &input_sender,
                            &mut input_database,
                            &clipboard_text,
                            &mut rendering_suspended,
                        ).await {
                            let _ = events.send(SessionEvent::Error(error)).await;
                        }
                    }
                    None => {
                        command_closed = true;
                        input_sender.request_close();
                    }
                }
            }
            maybe_output = output_receiver.recv() => {
                match maybe_output {
                    Some(output) => {
                        if let Some(reason) = map_output(
                            output,
                            &events,
                            &mut last_frame_sequence,
                            rendering_suspended,
                        ).await {
                            terminal_reason = Some(reason);
                            rdp_finished = true;
                        }
                    }
                    None => {
                        terminal_reason = Some(DisconnectReason::Backend(
                            "RDP output channel closed".into(),
                        ));
                        rdp_finished = true;
                    }
                }
            }
            maybe_clipboard = clipboard_receiver.recv() => {
                if let Some(message) = maybe_clipboard {
                    if input_sender.send_clipboard(message).is_err() {
                        terminal_reason = Some(DisconnectReason::Backend(
                            "clipboard channel closed".into(),
                        ));
                        rdp_finished = true;
                    }
                }
            }
            _ = &mut rdp_task, if !rdp_task_finished => {
                // RdpClient sends its final output event immediately before returning. Keep
                // receiving from the output channel so that Terminated/ConnectionFailure is
                // not lost when both branches become ready in the same select iteration.
                rdp_task_finished = true;
            }
        }

        if command_closed && rdp_finished {
            break;
        }
    }
    if let Ok(mut text) = clipboard_text.lock() {
        text.clear();
    }
    let reason = if user_disconnect_requested {
        DisconnectReason::UserRequested
    } else {
        terminal_reason.unwrap_or_else(|| DisconnectReason::Backend("RDP session ended".into()))
    };
    let _ = events.send(SessionEvent::Disconnected { reason }).await;
}

fn build_config(
    profile: &ConnectionProfile,
    password: &Secret,
    events: mpsc::Sender<SessionEvent>,
    clipboard_text: Arc<Mutex<String>>,
    clipboard_messages: mpsc::UnboundedSender<ClipboardMessage>,
) -> Result<(ironrdp_client::config::Config, TextClipboardFactory), SessionError> {
    let destination = Destination::new(profile.endpoint())
        .map_err(|error| SessionError::Backend(error.to_string()))?;
    let mut builder = ConfigBuilder::new()
        .with_destination(destination)
        .with_username(profile.username.clone())
        .with_password(password.expose())
        .with_client_build(RDP_CLIENT_BUILD)
        .with_client_dir(RDP_CLIENT_DIR)
        .with_platform(MajorPlatformType::WINDOWS)
        .with_client_name(RDP_CLIENT_NAME)
        .with_desktop_width(profile.desktop.width)
        .with_desktop_height(profile.desktop.height)
        .with_desktop_scale_factor(profile.desktop.scale_factor)
        .with_credssp(true)
        .with_tls(false)
        .with_server_pointer(true)
        .with_pointer_software_rendering(true)
        .with_transport(TransportKind::Direct)
        .with_compression(true)
        .with_clipboard(ClipboardType::Enable);

    if let Some(domain) = profile.domain.as_deref() {
        builder = builder.with_domain(domain);
    }

    builder = builder.with_certificate_validation(ironrdp_tls::CertificateValidation::Strict);
    if let CertificatePolicy::TrustOnFirstUse { fingerprint } = &profile.certificate_policy {
        let expected = fingerprint
            .clone()
            .map(|value| value.replace(':', "").to_ascii_lowercase());
        let events = events.clone();
        builder = builder.with_certificate_validation_callback(Arc::new(move |certificate, endpoint, error| {
            let actual = hex_sha256(certificate);
            match expected.as_deref() {
                Some(expected) if expected == actual => true,
                _ => {
                    debug!(endpoint, error, fingerprint = %actual, "RDP certificate requires trust confirmation");
                    let _ = events.try_send(SessionEvent::CertificateTrustRequired {
                        fingerprint: actual,
                    });
                    false
                }
            }
        }));
    }

    let factory = TextClipboardFactory {
        text: clipboard_text,
        events,
        messages: clipboard_messages,
    };
    let config = builder
        .build()
        .map_err(|error| SessionError::Backend(error.to_string()))?;
    Ok((config, factory))
}

fn hex_sha256(data: &[u8]) -> String {
    Sha256::digest(data)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

async fn dispatch_command(
    command: SessionCommand,
    sender: &RdpInputSender,
    database: &mut InputDatabase,
    clipboard_text: &Arc<Mutex<String>>,
    rendering_suspended: &mut bool,
) -> Result<(), SessionError> {
    match command {
        SessionCommand::Input(operation) => dispatch_operation(operation, sender, database).await,
        SessionCommand::PointerMove { x, y } => {
            dispatch_operation(InputOperation::PointerMove { x, y }, sender, database).await
        }
        SessionCommand::ButtonDown(button) => {
            dispatch_operation(InputOperation::ButtonDown(button), sender, database).await
        }
        SessionCommand::ButtonUp(button) => {
            dispatch_operation(InputOperation::ButtonUp(button), sender, database).await
        }
        SessionCommand::Wheel { vertical, units } => {
            dispatch_operation(InputOperation::Wheel { vertical, units }, sender, database).await
        }
        SessionCommand::KeyDown(key) => {
            dispatch_operation(InputOperation::KeyDown(key), sender, database).await
        }
        SessionCommand::KeyUp(key) => {
            dispatch_operation(InputOperation::KeyUp(key), sender, database).await
        }
        SessionCommand::SetLocalClipboard(value) => {
            let mut text = clipboard_text
                .lock()
                .map_err(|_| SessionError::Backend("clipboard lock poisoned".into()))?;
            *text = value;
            drop(text);
            sender
                .send_clipboard(ClipboardMessage::SendInitiateCopy(vec![
                    ClipboardFormat::new(ClipboardFormatId::CF_UNICODETEXT),
                ]))
                .map_err(|_| SessionError::ChannelClosed)
        }
        SessionCommand::RequestRemoteClipboard => sender
            .send_clipboard(ClipboardMessage::SendInitiatePaste(
                ClipboardFormatId::CF_UNICODETEXT,
            ))
            .map_err(|_| SessionError::ChannelClosed),
        SessionCommand::Resize {
            width,
            height,
            scale_factor,
        } => sender
            .try_send(RdpInputEvent::Resize {
                width,
                height,
                scale_factor,
                physical_size: None,
            })
            .map_err(map_input_send_error),
        SessionCommand::TrustCertificate { .. } => Err(SessionError::Backend(
            "certificate trust is applied when starting the next connection".into(),
        )),
        SessionCommand::SuspendRendering => {
            *rendering_suspended = true;
            Ok(())
        }
        SessionCommand::ResumeRendering => {
            *rendering_suspended = false;
            Ok(())
        }
        SessionCommand::Disconnect => {
            sender.request_close();
            Ok(())
        }
    }
}

async fn dispatch_operation(
    operation: InputOperation,
    sender: &RdpInputSender,
    database: &mut InputDatabase,
) -> Result<(), SessionError> {
    let operations = match operation {
        InputOperation::PointerMove { x, y } => vec![Operation::MouseMove(MousePosition { x, y })],
        InputOperation::ButtonDown(button) => {
            vec![Operation::MouseButtonPressed(to_iron_button(button))]
        }
        InputOperation::ButtonUp(button) => {
            vec![Operation::MouseButtonReleased(to_iron_button(button))]
        }
        InputOperation::Wheel { vertical, units } => {
            vec![Operation::WheelRotations(WheelRotations {
                is_vertical: vertical,
                rotation_units: units,
            })]
        }
        InputOperation::KeyDown(key) => vec![to_iron_key_operation(key, true)],
        InputOperation::KeyUp(key) => vec![to_iron_key_operation(key, false)],
        InputOperation::ClipboardText(_)
        | InputOperation::RequestRemoteClipboard
        | InputOperation::Resize(_)
        | InputOperation::Disconnect => return Ok(()),
    };
    let events = database.apply(operations);
    if events.is_empty() {
        return Ok(());
    }
    sender
        .try_send(RdpInputEvent::FastPath(events))
        .map_err(map_input_send_error)
}

fn map_input_send_error(
    error: tokio::sync::mpsc::error::TrySendError<RdpInputEvent>,
) -> SessionError {
    match error {
        tokio::sync::mpsc::error::TrySendError::Full(_) => {
            SessionError::Backend("RDP input queue is full".into())
        }
        tokio::sync::mpsc::error::TrySendError::Closed(_) => SessionError::ChannelClosed,
    }
}

fn to_iron_button(button: MouseButton) -> IronMouseButton {
    match button {
        MouseButton::Left => IronMouseButton::Left,
        MouseButton::Middle => IronMouseButton::Middle,
        MouseButton::Right => IronMouseButton::Right,
    }
}

fn to_iron_key_operation(key: KeyCode, pressed: bool) -> Operation {
    match key {
        KeyCode::Scancode { code, extended } => {
            if pressed {
                Operation::KeyPressed(Scancode::from_u8(extended, code))
            } else {
                Operation::KeyReleased(Scancode::from_u8(extended, code))
            }
        }
        KeyCode::Unicode(character) => {
            if pressed {
                Operation::UnicodeKeyPressed(character)
            } else {
                Operation::UnicodeKeyReleased(character)
            }
        }
    }
}

async fn map_output(
    output: RdpOutputEvent,
    events: &mpsc::Sender<SessionEvent>,
    last_sequence: &mut u64,
    rendering_suspended: bool,
) -> Option<DisconnectReason> {
    match output {
        RdpOutputEvent::Connected => {
            let _ = events
                .send(SessionEvent::StateChanged(SessionState::Connected))
                .await;
            None
        }
        RdpOutputEvent::Image {
            buffer,
            width,
            height,
        } => {
            let width = u32::from(width.get());
            let height = u32::from(height.get());
            if rendering_suspended {
                return None;
            }
            let mut rgba = Vec::with_capacity(buffer.len() * 4);
            for pixel in buffer {
                let [_, red, green, blue] = pixel.to_be_bytes();
                rgba.extend_from_slice(&[red, green, blue, 255]);
            }
            *last_sequence = last_sequence.saturating_add(1);
            match FrameUpdate::new(
                *last_sequence,
                width,
                height,
                PixelFormat::Rgba8888,
                rgba,
                vec![Rect::full(width, height)],
            ) {
                Ok(frame) => {
                    let _ = events.send(SessionEvent::Frame(Arc::new(frame))).await;
                }
                Err(error) => {
                    let _ = events
                        .send(SessionEvent::Error(SessionError::Backend(
                            error.to_string(),
                        )))
                        .await;
                }
            }
            None
        }
        RdpOutputEvent::AutoReconnecting {
            attempt,
            maximum_attempts,
            response,
            ..
        } => {
            let _ = events
                .send(SessionEvent::Reconnecting {
                    attempt,
                    maximum_attempts,
                })
                .await;
            let _ = response.send(AutoReconnectDecision::Continue);
            None
        }
        RdpOutputEvent::AutoReconnected => {
            let _ = events
                .send(SessionEvent::StateChanged(SessionState::Connected))
                .await;
            None
        }
        RdpOutputEvent::ConnectionFailure(error) => {
            warn!(error = %error, "RDP connection failed");
            let message = error.report().to_string();
            let _ = events
                .send(SessionEvent::Error(SessionError::Backend(message.clone())))
                .await;
            Some(DisconnectReason::Backend(message))
        }
        RdpOutputEvent::MonitorLayout(monitors) => {
            if let Some((width, height)) = monitor_bounds(&monitors) {
                let _ = events.send(SessionEvent::Connected { width, height }).await;
            }
            None
        }
        RdpOutputEvent::Terminated(result) => {
            debug!(result = ?result, "RDP session terminated");
            Some(DisconnectReason::Backend(format!(
                "RDP client terminated: {result:?}"
            )))
        }
        RdpOutputEvent::PointerDefault
        | RdpOutputEvent::PointerHidden
        | RdpOutputEvent::PointerPosition { .. }
        | RdpOutputEvent::PointerBitmap(_)
        | RdpOutputEvent::LoginComplete
        | RdpOutputEvent::PostLogonDisplayRedraw
        | RdpOutputEvent::MalformedBitmapDisplayRedraw
        | RdpOutputEvent::DisplayResizeFallback(_)
        | RdpOutputEvent::WindowingOrders(_)
        | RdpOutputEvent::RailHandshake { .. }
        | RdpOutputEvent::RailDesktopSynchronized { .. }
        | RdpOutputEvent::RailPostHandshakeQueueReleased { .. }
        | RdpOutputEvent::RailExecuteResult(_)
        | RdpOutputEvent::RailExecuteFailed { .. }
        | RdpOutputEvent::RailApplicationId { .. }
        | RdpOutputEvent::RailControl(_) => None,
    }
}

fn monitor_bounds(monitors: &[Monitor]) -> Option<(u32, u32)> {
    let left = monitors
        .iter()
        .map(|monitor| i64::from(monitor.left))
        .min()?;
    let top = monitors
        .iter()
        .map(|monitor| i64::from(monitor.top))
        .min()?;
    let right = monitors
        .iter()
        .map(|monitor| i64::from(monitor.right))
        .max()?;
    let bottom = monitors
        .iter()
        .map(|monitor| i64::from(monitor.bottom))
        .max()?;
    let width = u32::try_from((right - left).max(0)).ok()?;
    let height = u32::try_from((bottom - top).max(0)).ok()?;
    (width > 0 && height > 0).then_some((width, height))
}

#[derive(Debug, Clone)]
struct TextClipboardFactory {
    text: Arc<Mutex<String>>,
    events: mpsc::Sender<SessionEvent>,
    messages: mpsc::UnboundedSender<ClipboardMessage>,
}

impl CliprdrBackendFactory for TextClipboardFactory {
    fn build_cliprdr_backend(&self) -> Box<dyn CliprdrBackend> {
        Box::new(TextClipboardBackend {
            text: self.text.clone(),
            events: self.events.clone(),
            messages: self.messages.clone(),
        })
    }
}

#[derive(Debug)]
struct TextClipboardBackend {
    text: Arc<Mutex<String>>,
    events: mpsc::Sender<SessionEvent>,
    messages: mpsc::UnboundedSender<ClipboardMessage>,
}

impl_as_any!(TextClipboardBackend);

impl CliprdrBackend for TextClipboardBackend {
    fn temporary_directory(&self) -> &str {
        ""
    }

    fn client_capabilities(&self) -> ClipboardGeneralCapabilityFlags {
        ClipboardGeneralCapabilityFlags::empty()
    }

    fn on_ready(&mut self) {}

    fn on_request_format_list(&mut self) {
        let _ = self.messages.send(ClipboardMessage::SendInitiateCopy(vec![
            ClipboardFormat::new(ClipboardFormatId::CF_UNICODETEXT),
        ]));
    }

    fn on_process_negotiated_capabilities(
        &mut self,
        _capabilities: ClipboardGeneralCapabilityFlags,
    ) {
    }

    fn on_remote_copy(&mut self, _available_formats: &[ClipboardFormat]) {}

    fn on_format_data_request(&mut self, request: FormatDataRequest) {
        let text = self
            .text
            .lock()
            .map(|value| value.clone())
            .unwrap_or_default();
        let response = if request.format == ClipboardFormatId::CF_UNICODETEXT {
            FormatDataResponse::new_unicode_string(&text).into_owned()
        } else if request.format == ClipboardFormatId::CF_TEXT {
            FormatDataResponse::new_string(&text).into_owned()
        } else {
            FormatDataResponse::new_error().into_owned()
        };
        let _ = self
            .messages
            .send(ClipboardMessage::SendFormatData(response));
    }

    fn on_format_data_response(&mut self, response: FormatDataResponse<'_>) {
        if response.is_error() {
            return;
        }
        match response
            .to_unicode_string()
            .or_else(|_| response.to_string())
        {
            Ok(value) => {
                let _ = self.events.try_send(SessionEvent::ClipboardText(value));
            }
            Err(error) => {
                let _ = self
                    .events
                    .try_send(SessionEvent::Error(SessionError::Backend(
                        error.to_string(),
                    )));
            }
        }
    }

    fn on_file_contents_request(&mut self, _request: ironrdp_cliprdr::pdu::FileContentsRequest) {}

    fn on_file_contents_response(
        &mut self,
        _response: ironrdp_cliprdr::pdu::FileContentsResponse<'_>,
    ) {
    }

    fn on_lock(&mut self, _data_id: ironrdp_cliprdr::pdu::LockDataId) {}

    fn on_unlock(&mut self, _data_id: ironrdp_cliprdr::pdu::LockDataId) {}
}
