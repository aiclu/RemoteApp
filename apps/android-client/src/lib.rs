use std::sync::{
    Arc, Mutex,
    atomic::{AtomicU64, Ordering},
};
use std::thread;

use remoteapp_rdp_core::{
    CertificatePolicy, ConnectionProfile, DisconnectReason, EndpointParseError, FrameUpdate,
    ReconnectPolicy, Secret, SessionCommand, SessionEvent, SessionHandle, SessionStart,
    SessionState, parse_endpoint, spawn_session,
};
use slint::{Image, Rgba8Pixel, SharedPixelBuffer, SharedString, Weak};
use tokio::sync::mpsc::{Receiver, UnboundedSender};

slint::include_modules!();

type CommandSender = UnboundedSender<SessionCommand>;

#[derive(Clone, Default)]
struct SessionController {
    commands: Arc<Mutex<Option<CommandSender>>>,
    credentials: Arc<Mutex<Option<(ConnectionProfile, Secret)>>>,
    generation: Arc<AtomicU64>,
}

impl SessionController {
    fn remember_credentials(&self, profile: ConnectionProfile, password: Secret) {
        if let Ok(mut credentials) = self.credentials.lock() {
            *credentials = Some((profile, password));
        }
    }

    fn trusted_start(&self, fingerprint: String) -> Option<SessionStart> {
        let (mut profile, password) = self.credentials.lock().ok()?.clone()?;
        profile.certificate_policy = CertificatePolicy::TrustOnFirstUse {
            fingerprint: Some(fingerprint),
        };
        Some(SessionStart {
            profile,
            password,
            reconnect: ReconnectPolicy::default(),
        })
    }

    fn begin(&self, commands: CommandSender) -> u64 {
        let generation = self
            .generation
            .fetch_add(1, Ordering::SeqCst)
            .saturating_add(1);
        let previous = self
            .commands
            .lock()
            .ok()
            .and_then(|mut current| current.replace(commands));
        if let Some(previous) = previous {
            let _ = previous.send(SessionCommand::Disconnect);
        }
        generation
    }

    fn send(&self, command: SessionCommand) {
        if let Some(sender) = self
            .commands
            .lock()
            .ok()
            .and_then(|current| current.clone())
        {
            let _ = sender.send(command);
        }
    }

    fn clear_if_current(&self, generation: u64) {
        if self.generation.load(Ordering::SeqCst) == generation {
            if let Ok(mut current) = self.commands.lock() {
                current.take();
            }
        }
    }
}

pub fn run() -> Result<(), slint::PlatformError> {
    let ui = MainWindow::new()?;
    let controller = SessionController::default();

    {
        let controller = controller.clone();
        let ui_weak = ui.as_weak();
        ui.on_connect_requested(move || {
            let Some(ui) = ui_weak.upgrade() else {
                return;
            };
            let endpoint = ui.get_host().trim().to_owned();
            let username = ui.get_username().trim().to_owned();
            let password = ui.get_password().to_string();
            if endpoint.is_empty() || username.is_empty() || password.is_empty() {
                ui.set_status("请填写主机、用户名和密码".into());
                return;
            }
            let (host, port) = match parse_endpoint(&endpoint) {
                Ok(value) => value,
                Err(error) => {
                    ui.set_status(endpoint_error_text(error).into());
                    return;
                }
            };

            let profile = ConnectionProfile {
                label: endpoint,
                host,
                port,
                username,
                certificate_policy: CertificatePolicy::TrustOnFirstUse { fingerprint: None },
                ..Default::default()
            };
            let start = SessionStart {
                profile,
                password: Secret::new(password),
                reconnect: ReconnectPolicy::default(),
            };
            ui.set_password("".into());
            ui.set_pending_fingerprint("".into());
            ui.set_status("正在连接…".into());
            launch_session(&controller, ui_weak.clone(), start);
        });
    }

    {
        let controller = controller.clone();
        ui.on_disconnect_requested(move || controller.send(SessionCommand::Disconnect));
    }

    {
        let controller = controller.clone();
        ui.on_left_click(move || {
            controller.send(SessionCommand::ButtonDown(
                remoteapp_rdp_core::MouseButton::Left,
            ));
            controller.send(SessionCommand::ButtonUp(
                remoteapp_rdp_core::MouseButton::Left,
            ));
        });
    }

    {
        let controller = controller.clone();
        ui.on_right_click(move || {
            controller.send(SessionCommand::ButtonDown(
                remoteapp_rdp_core::MouseButton::Right,
            ));
            controller.send(SessionCommand::ButtonUp(
                remoteapp_rdp_core::MouseButton::Right,
            ));
        });
    }

    {
        let controller = controller.clone();
        ui.on_scroll(move |units| {
            controller.send(SessionCommand::Wheel {
                vertical: true,
                units: units.clamp(-32768, 32767) as i16,
            });
        });
    }

    {
        let controller = controller.clone();
        ui.on_touch_moved(move |x, y| {
            controller.send(SessionCommand::PointerMove {
                x: map_touch_coordinate(x, 320.0, 1919),
                y: map_touch_coordinate(y, 180.0, 1079),
            });
        });
    }

    {
        let controller = controller.clone();
        ui.on_touch_tapped(move || {
            controller.send(SessionCommand::ButtonDown(
                remoteapp_rdp_core::MouseButton::Left,
            ));
            controller.send(SessionCommand::ButtonUp(
                remoteapp_rdp_core::MouseButton::Left,
            ));
        });
    }

    {
        let controller = controller.clone();
        let ui_weak = ui.as_weak();
        ui.on_send_clipboard(move || {
            if let Some(ui) = ui_weak.upgrade() {
                controller.send(SessionCommand::SetLocalClipboard(
                    ui.get_clipboard().to_string(),
                ));
                ui.set_status("已发送剪贴板内容".into());
            }
        });
    }

    {
        let controller = controller.clone();
        let ui_weak = ui.as_weak();
        ui.on_request_clipboard(move || {
            controller.send(SessionCommand::RequestRemoteClipboard);
            set_status(&ui_weak, "正在请求远端剪贴板…");
        });
    }

    {
        let controller = controller.clone();
        let ui_weak = ui.as_weak();
        ui.on_trust_certificate(move |fingerprint| {
            let Some(start) = controller.trusted_start(fingerprint.to_string()) else {
                set_status(&ui_weak, "没有可重连的会话凭据，请重新输入密码");
                return;
            };
            set_status(&ui_weak, "已记录证书指纹，正在重连…");
            launch_session(&controller, ui_weak.clone(), start);
        });
    }

    ui.run()
}

fn launch_session(controller: &SessionController, ui: Weak<MainWindow>, start: SessionStart) {
    controller.remember_credentials(start.profile.clone(), start.password.clone());
    match spawn_session(start) {
        Ok(SessionHandle { commands, events }) => {
            let generation = controller.begin(commands);
            monitor_events(ui, controller.clone(), generation, events);
        }
        Err(error) => set_status(&ui, format!("连接启动失败：{error}")),
    }
}

fn monitor_events(
    ui: Weak<MainWindow>,
    controller: SessionController,
    generation: u64,
    mut events: Receiver<SessionEvent>,
) {
    let _ = thread::Builder::new()
        .name("remoteapp-ui-events".to_owned())
        .spawn(move || {
            let runtime = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(runtime) => runtime,
                Err(error) => {
                    set_status(&ui, format!("事件线程启动失败：{error}"));
                    controller.clear_if_current(generation);
                    return;
                }
            };
            runtime.block_on(async move {
                while let Some(event) = events.recv().await {
                    match event {
                        SessionEvent::StateChanged(state) => {
                            set_status(&ui, session_state_text(state));
                        }
                        SessionEvent::Connected { width, height } => {
                            let weak = ui.clone();
                            let _ = slint::invoke_from_event_loop(move || {
                                if let Some(ui) = weak.upgrade() {
                                    ui.set_pending_fingerprint("".into());
                                    ui.set_status(format!("已连接 · {width}×{height}").into());
                                }
                            });
                        }
                        SessionEvent::Frame(frame) => enqueue_frame(&ui, &frame),
                        SessionEvent::ClipboardText(text) => {
                            let text: SharedString = text.into();
                            let weak = ui.clone();
                            let _ = slint::invoke_from_event_loop(move || {
                                if let Some(ui) = weak.upgrade() {
                                    ui.set_clipboard(text);
                                    ui.set_status("已收到远端剪贴板内容".into());
                                }
                            });
                        }
                        SessionEvent::CertificateTrustRequired { fingerprint } => {
                            let weak = ui.clone();
                            let _ = slint::invoke_from_event_loop(move || {
                                if let Some(ui) = weak.upgrade() {
                                    ui.set_pending_fingerprint(fingerprint.clone().into());
                                    ui.set_status(format!("证书待确认：{fingerprint}").into());
                                }
                            });
                        }
                        SessionEvent::Reconnecting {
                            attempt,
                            maximum_attempts,
                        } => {
                            set_status(
                                &ui,
                                format!("网络中断，重连 {attempt}/{maximum_attempts}…"),
                            );
                        }
                        SessionEvent::Disconnected { reason } => {
                            set_status(&ui, disconnect_reason_text(reason));
                        }
                        SessionEvent::Error(error) => set_status(&ui, format!("错误：{error}")),
                    }
                }
                controller.clear_if_current(generation);
            });
        });
}

fn enqueue_frame(ui: &Weak<MainWindow>, frame: &FrameUpdate) {
    let mut pixels = SharedPixelBuffer::<Rgba8Pixel>::new(frame.width, frame.height);
    pixels
        .make_mut_bytes()
        .copy_from_slice(frame.buffer.as_bytes());
    let weak = ui.clone();
    let width = frame.width;
    let height = frame.height;
    let _ = slint::invoke_from_event_loop(move || {
        if let Some(ui) = weak.upgrade() {
            ui.set_remote_image(Image::from_rgba8(pixels));
            ui.set_status(format!("已连接 · {width}×{height}").into());
        }
    });
}

fn set_status(ui: &Weak<MainWindow>, status: impl Into<SharedString>) {
    let status = status.into();
    let weak = ui.clone();
    let _ = slint::invoke_from_event_loop(move || {
        if let Some(ui) = weak.upgrade() {
            ui.set_status(status);
        }
    });
}

fn map_touch_coordinate(value: f32, local_extent: f32, remote_max: u16) -> u16 {
    let normalized = (value / local_extent).clamp(0.0, 1.0);
    (normalized * f32::from(remote_max)).round() as u16
}

fn session_state_text(state: SessionState) -> &'static str {
    match state {
        SessionState::Idle => "空闲",
        SessionState::Connecting => "正在连接…",
        SessionState::Connected => "已连接",
        SessionState::Reconnecting => "正在重连…",
        SessionState::Disconnecting => "正在断开…",
        SessionState::Disconnected => "已断开",
    }
}

fn endpoint_error_text(error: EndpointParseError) -> &'static str {
    match error {
        EndpointParseError::MissingHost => "主机地址不能为空",
        EndpointParseError::InvalidHost => "主机地址格式无效",
        EndpointParseError::InvalidPort => "端口必须是 1 到 65535",
    }
}

fn disconnect_reason_text(reason: DisconnectReason) -> String {
    match reason {
        DisconnectReason::UserRequested => "已断开".into(),
        DisconnectReason::AuthenticationFailed => "认证失败，请检查用户名和密码".into(),
        DisconnectReason::CertificateRejected => "证书被拒绝".into(),
        DisconnectReason::TransportLost => "网络连接已断开".into(),
        DisconnectReason::ProtocolError => "RDP 协议错误".into(),
        DisconnectReason::Backend(message) => format!("连接失败：{message}"),
    }
}

#[cfg(target_os = "android")]
#[unsafe(no_mangle)]
pub fn android_main(app: slint::android::AndroidApp) {
    slint::android::init(app).expect("failed to initialize Slint Android backend");
    run().expect("RemoteAPP UI failed");
}
