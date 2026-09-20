mod controller;
mod interaction;
mod platform;
mod repository;

use controller::Controller;
use interaction::{Gestures, committed_delta, key_code, shortcut_key};
use platform::{NativePlatform, PlatformServices};
use remoteapp_rdp_core::{
    CertificatePolicy, ConnectionProfile, DesktopConfig, KeyCode, ProfileId, ReconnectPolicy,
    Secret, SessionCommand, SessionEvent, SessionStart, SessionState, parse_endpoint,
    spawn_session,
};
use repository::{Device, DeviceRepository, endpoint_key};
use slint::{
    ComponentHandle, Image, ModelRc, Rgba8Pixel, SharedPixelBuffer, Timer, TimerMode, VecModel,
};
use std::{
    cell::RefCell,
    collections::HashMap,
    rc::Rc,
    time::{Duration, Instant},
};

slint::include_modules!();

struct AppModel {
    repository: Option<DeviceRepository>,
    draft: Option<ProfileId>,
    gestures: Gestures,
    ime_previous: String,
    pressed: HashMap<String, KeyCode>,
    latched: HashMap<String, KeyCode>,
    dynamic_resolution: bool,
    desktop_scale: u32,
    resize: Option<(Instant, u16, u16)>,
    last_size: Option<(u16, u16)>,
    last_frame: (u64, u64),
}
impl AppModel {
    fn new(repository: Option<DeviceRepository>) -> Self {
        Self {
            repository,
            draft: None,
            gestures: Gestures::default(),
            ime_previous: String::new(),
            pressed: HashMap::new(),
            latched: HashMap::new(),
            dynamic_resolution: true,
            desktop_scale: 100,
            resize: None,
            last_size: None,
            last_frame: (0, 0),
        }
    }
}
fn feedback(ui: &MainWindow, text: impl Into<slint::SharedString>, error: bool) {
    ui.set_status(text.into());
    ui.set_error(error);
}
fn result(ui: &MainWindow, value: Result<(), String>, success: &str) {
    match value {
        Ok(()) => feedback(ui, success, false),
        Err(error) => feedback(ui, error, true),
    }
}
fn refresh(ui: &MainWindow, model: &AppModel) {
    let search = ui.get_search().to_lowercase();
    let mut rows = model
        .repository
        .as_ref()
        .map(|r| {
            r.catalog
                .devices
                .iter()
                .filter(|d| {
                    (!ui.get_favorites_only() || d.favorite)
                        && format!(
                            "{} {} {}",
                            d.profile.label, d.profile.host, d.profile.username
                        )
                        .to_lowercase()
                        .contains(&search)
                })
                .map(|d| DeviceRow {
                    id: d.profile.id.to_string().into(),
                    name: d.profile.label.clone().into(),
                    endpoint: d.profile.endpoint().into(),
                    username: d.profile.username.clone().into(),
                    favorite: d.favorite,
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    rows.sort_by(|a, b| {
        b.favorite
            .cmp(&a.favorite)
            .then_with(|| a.name.cmp(&b.name))
    });
    ui.set_devices(ModelRc::new(VecModel::from(rows)));
}
fn find_device(model: &AppModel, id: &str) -> Option<Device> {
    model
        .repository
        .as_ref()?
        .catalog
        .devices
        .iter()
        .find(|d| d.profile.id.to_string() == id)
        .cloned()
}
fn edit(ui: &MainWindow, model: &mut AppModel, device: Option<Device>) {
    let d = device.unwrap_or(Device {
        profile: ConnectionProfile::default(),
        favorite: false,
        remember_password: false,
        password: None,
        direct_touch: false,
        dynamic_resolution: true,
    });
    model.draft = Some(d.profile.id);
    ui.set_device_name(d.profile.label.clone().into());
    ui.set_host(d.profile.host.clone().into());
    ui.set_port(d.profile.port.to_string().into());
    ui.set_username(
        d.profile
            .domain
            .as_ref()
            .map(|domain| format!("{domain}\\{}", d.profile.username))
            .unwrap_or_else(|| d.profile.username.clone())
            .into(),
    );
    ui.set_password(d.password.clone().unwrap_or_default().into());
    ui.set_remember_password(d.remember_password);
    ui.set_direct_touch(d.direct_touch);
    ui.set_dynamic_resolution(d.dynamic_resolution);
    ui.set_desktop_width(d.profile.desktop.width.to_string().into());
    ui.set_desktop_height(d.profile.desktop.height.to_string().into());
    ui.set_desktop_scale(d.profile.desktop.scale_factor.to_string().into());
    ui.set_page(1);
    feedback(ui, "", false);
}
fn save(ui: &MainWindow, model: &mut AppModel) -> Result<Device, String> {
    let (host, inline_port) = parse_endpoint(ui.get_host().as_str()).map_err(|e| e.to_string())?;
    let port = ui
        .get_port()
        .parse::<u16>()
        .map_err(|_| "端口必须为 1–65535")?;
    let port = if port == 3389 { inline_port } else { port };
    let username = ui.get_username().trim().to_owned();
    let (domain, username) = username
        .split_once('\\')
        .map(|(d, u)| (Some(d.to_owned()), u.to_owned()))
        .unwrap_or((None, username.clone()));
    let mut profile = ConnectionProfile {
        id: model.draft.unwrap_or_default(),
        label: ui.get_device_name().trim().to_owned(),
        host,
        port,
        username,
        domain,
        desktop: DesktopConfig {
            width: ui
                .get_desktop_width()
                .parse()
                .map_err(|_| "请输入有效的桌面宽度")?,
            height: ui
                .get_desktop_height()
                .parse()
                .map_err(|_| "请输入有效的桌面高度")?,
            scale_factor: ui
                .get_desktop_scale()
                .parse()
                .map_err(|_| "请输入有效的缩放百分比")?,
        },
        ..Default::default()
    };
    if profile.label.is_empty() {
        profile.label = profile.endpoint();
    }
    profile.validate().map_err(|e| e.to_string())?;
    let repo = model
        .repository
        .as_mut()
        .ok_or("安全存储不可用，无法保存")?;
    let device = Device {
        favorite: repo.device(profile.id).is_some_and(|d| d.favorite),
        profile,
        remember_password: ui.get_remember_password(),
        password: if ui.get_remember_password() && !ui.get_password().is_empty() {
            Some(ui.get_password().to_string())
        } else {
            None
        },
        direct_touch: ui.get_direct_touch(),
        dynamic_resolution: ui.get_dynamic_resolution(),
    };
    repo.update(|c| {
        if let Some(d) = c
            .devices
            .iter_mut()
            .find(|d| d.profile.id == device.profile.id)
        {
            *d = device.clone();
        } else {
            c.devices.push(device.clone());
        }
    })?;
    refresh(ui, model);
    Ok(device)
}
fn defer_focus(ui: &MainWindow, keyboard: bool) {
    let weak = ui.as_weak();
    Timer::single_shot(Duration::ZERO, move || {
        if let Some(ui) = weak.upgrade() {
            if keyboard {
                ui.invoke_focus_keyboard();
            } else {
                ui.invoke_focus_session();
            }
        }
    });
}
fn update_view(ui: &MainWindow, model: &AppModel) {
    let v = &model.gestures.viewport;
    let (x, y) = v.origin();
    ui.set_image_x(x);
    ui.set_image_y(y);
    ui.set_image_width(v.remote_width * v.scale());
    ui.set_image_height(v.remote_height * v.scale());
    ui.set_zoom(v.zoom);
}
fn release(model: &mut AppModel, controller: &Controller) {
    model.gestures.cancel();
    model.pressed.clear();
    model.latched.clear();
    controller.release_inputs();
}
fn stop(ui: &MainWindow, model: &mut AppModel, controller: &Controller) {
    release(model, controller);
    controller.disconnect();
    model.resize = None;
    model.ime_previous.clear();
    ui.set_ime_text("".into());
    ui.set_password("".into());
    ui.set_connected(false);
    ui.set_pending_fingerprint("".into());
    ui.set_remote_image(Image::default());
    ui.set_keyboard_visible(false);
    ui.set_clipboard_visible(false);
    ui.set_clipboard("".into());
    ui.set_page(0);
    feedback(ui, "已断开", false);
    refresh(ui, model);
}
fn launch(
    ui: &MainWindow,
    model: &mut AppModel,
    controller: &Controller,
    device: &Device,
    password: Secret,
) -> Result<(), String> {
    if password.is_empty() {
        return Err("请输入连接密码".into());
    }
    let mut profile = device.profile.clone();
    let fingerprint = model
        .repository
        .as_ref()
        .and_then(|r| r.catalog.trust.get(&endpoint_key(&profile)).cloned());
    profile.certificate_policy = CertificatePolicy::TrustOnFirstUse { fingerprint };
    let start = SessionStart {
        profile: profile.clone(),
        password: password.clone(),
        reconnect: ReconnectPolicy::default(),
    };
    let handle = spawn_session(start).map_err(|e| e.to_string())?;
    release(model, controller);
    controller.begin(profile.clone(), password, handle);
    let (view_width, view_height) = (
        model.gestures.viewport.width,
        model.gestures.viewport.height,
    );
    model.gestures.reset();
    model.gestures.direct = device.direct_touch;
    model.gestures.viewport.width = view_width;
    model.gestures.viewport.height = view_height;
    model.gestures.viewport.remote_width = f32::from(profile.desktop.width);
    model.gestures.viewport.remote_height = f32::from(profile.desktop.height);
    model.dynamic_resolution = device.dynamic_resolution;
    model.desktop_scale = profile.desktop.scale_factor;
    model.last_frame = (0, 0);
    model.last_size = None;
    model.ime_previous.clear();
    ui.set_remote_image(Image::default());
    ui.set_password("".into());
    ui.set_ime_text("".into());
    ui.set_clipboard("".into());
    ui.set_pending_fingerprint("".into());
    ui.set_connected(false);
    ui.set_direct_touch(device.direct_touch);
    ui.set_pan_mode(false);
    ui.set_keyboard_visible(false);
    ui.set_clipboard_visible(false);
    ui.set_session_name(profile.label.into());
    ui.set_page(2);
    feedback(ui, "正在连接…", false);
    defer_focus(ui, false);
    Ok(())
}
fn send_batch(ui: &MainWindow, controller: &Controller, commands: Vec<SessionCommand>) {
    for command in commands {
        if let Err(error) = controller.send(command) {
            feedback(ui, error, true);
            break;
        }
    }
}
fn remote_key(
    model: &mut AppModel,
    controller: &Controller,
    text: &str,
    down: bool,
    ctrl: bool,
    alt: bool,
    shift: bool,
    meta: bool,
) -> bool {
    if !controller.0.lock().unwrap().connected {
        return false;
    }
    if !down {
        if let Some(key) = model.pressed.remove(text) {
            let _ = controller.send(SessionCommand::KeyUp(key));
        }
        return true;
    }
    let modifier_held = model
        .pressed
        .iter()
        .filter(|(name, _)| !name.starts_with("modifier:"))
        .map(|(_, key)| key)
        .chain(model.latched.values())
        .any(|key| {
            matches!(
                key,
                KeyCode::Scancode {
                    code: 0x1d | 0x38 | 0x5b,
                    ..
                }
            )
        });
    let key = if ctrl || alt || meta || modifier_held {
        shortcut_key(text)
    } else {
        key_code(text)
    };
    let Some(key) = key else {
        return false;
    };
    // Synchronize modifiers before the key: backend events may omit separate modifier presses.
    for (name, enabled, code, extended) in [
        ("Control", ctrl, 0x1d, false),
        ("Alt", alt, 0x38, false),
        ("Shift", shift, 0x2a, false),
        ("Meta", meta, 0x5b, true),
    ] {
        let token = format!("modifier:{name}");
        if enabled && !model.pressed.contains_key(&token) {
            let key = KeyCode::Scancode { code, extended };
            let _ = controller.send(SessionCommand::KeyDown(key));
            model.pressed.insert(token, key);
        } else if !enabled {
            if let Some(key) = model.pressed.remove(&token) {
                let _ = controller.send(SessionCommand::KeyUp(key));
            }
        }
    }
    if let Some(old) = model.pressed.insert(text.to_owned(), key) {
        let _ = controller.send(SessionCommand::KeyUp(old));
    }
    let _ = controller.send(SessionCommand::KeyDown(key));
    true
}

fn prepare_app(
    controller: Controller,
    repository: Result<DeviceRepository, String>,
) -> Result<(MainWindow, Timer), slint::PlatformError> {
    let ui = MainWindow::new()?;

    ui.set_storage_ready(repository.is_ok());
    let repository = match repository {
        Ok(r) => Some(r),
        Err(e) => {
            feedback(&ui, format!("安全存储不可用：{e}"), true);
            None
        }
    };
    let model = Rc::new(RefCell::new(AppModel::new(repository)));
    refresh(&ui, &model.borrow());
    macro_rules! bind {
        ($callback:ident, |$u:ident,$m:ident,$c:ident $(,$arg:ident)*| $body:block) => {{
            let weak=ui.as_weak();let model=model.clone();let controller=controller.clone();
            ui.$callback(move |$($arg),*|{if let Some($u)=weak.upgrade(){let mut state=model.borrow_mut();let $m=&mut *state;let $c=&controller; $body }});
        }};
    }
    bind!(on_new_device, |u, m, _c| {
        edit(&u, m, None);
    });
    bind!(on_edit_device, |u, m, _c, id| {
        if let Some(d) = find_device(m, &id) {
            edit(&u, m, Some(d));
        }
    });
    bind!(on_filter_changed, |u, m, _c| {
        refresh(&u, m);
    });
    bind!(on_leave_editor, |u, _m, _c| {
        u.set_password("".into());
        u.set_page(0);
        feedback(&u, "", false);
    });
    bind!(on_toggle_favorite, |u, m, _c, id| {
        let value = m
            .repository
            .as_mut()
            .ok_or("安全存储不可用".to_owned())
            .and_then(|r| {
                r.update(|catalog| {
                    if let Some(d) = catalog
                        .devices
                        .iter_mut()
                        .find(|d| d.profile.id.to_string() == id.as_str())
                    {
                        d.favorite = !d.favorite;
                    }
                })
            });
        result(&u, value, "");
        refresh(&u, m);
    });
    bind!(on_delete_device, |u, m, _c, id| {
        let value = m
            .repository
            .as_mut()
            .ok_or("安全存储不可用".to_owned())
            .and_then(|r| {
                r.update(|catalog| {
                    let endpoint = catalog
                        .devices
                        .iter()
                        .find(|d| d.profile.id.to_string() == id.as_str())
                        .map(|d| endpoint_key(&d.profile));
                    catalog
                        .devices
                        .retain(|d| d.profile.id.to_string() != id.as_str());
                    if let Some(endpoint) = endpoint {
                        if !catalog
                            .devices
                            .iter()
                            .any(|d| endpoint_key(&d.profile) == endpoint)
                        {
                            catalog.trust.remove(&endpoint);
                        }
                    }
                })
            });
        result(&u, value, "设备已删除");
        refresh(&u, m);
    });
    bind!(on_save_device, |u, m, c, connect| {
        if connect && u.get_password().is_empty() {
            feedback(&u, "请输入连接密码", true);
            return;
        }
        match save(&u, m) {
            Ok(device) => {
                if connect {
                    let password = Secret::new(u.get_password().to_string());
                    if let Err(e) = launch(&u, m, c, &device, password) {
                        feedback(&u, e, true);
                    }
                } else {
                    u.set_password("".into());
                    u.set_page(0);
                    feedback(&u, "设备已保存", false);
                }
            }
            Err(e) => feedback(&u, e, true),
        }
    });
    bind!(on_connect_device, |u, m, c, id| {
        if let Some(device) = find_device(m, &id) {
            if let Some(password) = device.password.as_ref().filter(|p| !p.is_empty()) {
                if let Err(e) = launch(&u, m, c, &device, Secret::new(password.clone())) {
                    feedback(&u, e, true);
                }
            } else {
                edit(&u, m, Some(device));
                feedback(&u, "输入密码后选择“保存并连接”", false);
            }
        }
    });
    bind!(on_disconnect, |u, m, c| {
        stop(&u, m, c);
    });
    bind!(on_reject_certificate, |u, m, c| {
        stop(&u, m, c);
    });
    bind!(on_trust_certificate, |u, m, c| {
        let pending = {
            let active = c.0.lock().unwrap();
            active
                .credentials
                .clone()
                .zip(active.pending_fingerprint.clone())
                .map(|(credentials, fp)| (active.generation, credentials, fp))
        };
        if let Some((generation, (profile, password), fingerprint)) = pending {
            if !c.current(generation) {
                return;
            }
            let value = m
                .repository
                .as_mut()
                .ok_or("安全存储不可用".to_owned())
                .and_then(|r| {
                    r.update(|catalog| {
                        catalog.trust.insert(endpoint_key(&profile), fingerprint);
                    })
                });
            if let Err(e) = value {
                feedback(&u, e, true);
                return;
            }
            let device = m
                .repository
                .as_ref()
                .and_then(|r| r.device(profile.id))
                .cloned();
            if let Some(device) = device {
                if let Err(e) = launch(&u, m, c, &device, password) {
                    feedback(&u, e, true);
                }
            }
        }
    });
    bind!(on_pointer, |u, m, c, id, kind, x, y, button| {
        if kind == 0 {
            m.ime_previous.clear();
            u.set_ime_text("".into());
        }
        let commands = m.gestures.event(id, kind, x, y, button, Instant::now());
        send_batch(&u, c, commands);
        update_view(&u, m);
    });
    bind!(on_wheel, |u, _m, c, x, y| {
        if y != 0. {
            result(
                &u,
                c.send(SessionCommand::Wheel {
                    vertical: true,
                    units: y.clamp(-32767., 32767.) as i16,
                }),
                "",
            );
        }
        if x != 0. {
            result(
                &u,
                c.send(SessionCommand::Wheel {
                    vertical: false,
                    units: x.clamp(-32767., 32767.) as i16,
                }),
                "",
            );
        }
    });
    bind!(on_viewport, |u, m, _c, width, height| {
        if width < 1. || height < 1. {
            return;
        }
        m.gestures.viewport.width = width;
        m.gestures.viewport.height = height;
        m.gestures.viewport.clamp_pan();
        update_view(&u, m);
        let scale = u.window().scale_factor();
        let size = (
            (width * scale).round().clamp(320., 8192.) as u16,
            (height * scale).round().clamp(200., 8192.) as u16,
        );
        if m.dynamic_resolution && m.last_size != Some(size) {
            m.resize = Some((Instant::now() + Duration::from_millis(250), size.0, size.1));
        }
    });
    {
        let model = model.clone();
        let controller = controller.clone();
        ui.on_release_input(move || {
            controller.release_inputs();
            if let Ok(mut state) = model.try_borrow_mut() {
                state.gestures.cancel();
                state.pressed.clear();
                state.latched.clear();
            } else {
                let model = model.clone();
                Timer::single_shot(Duration::ZERO, move || {
                    let mut state = model.borrow_mut();
                    state.gestures.cancel();
                    state.pressed.clear();
                    state.latched.clear();
                });
            }
        });
    }
    bind!(on_input_mode_changed, |u, m, c| {
        release(m, c);
        m.gestures.direct = u.get_direct_touch();
        m.gestures.pan_mode = u.get_pan_mode();
    });
    bind!(on_reset_view, |u, m, c| {
        release(m, c);
        m.gestures.viewport.zoom = 1.;
        m.gestures.viewport.pan_x = 0.;
        m.gestures.viewport.pan_y = 0.;
        update_view(&u, m);
    });
    {
        let model = model.clone();
        let controller = controller.clone();
        ui.on_key(move |text, down, ctrl, alt, shift, meta| {
            remote_key(
                &mut model.borrow_mut(),
                &controller,
                &text,
                down,
                ctrl,
                alt,
                shift,
                meta,
            )
        });
    }
    {
        let model = model.clone();
        let controller = controller.clone();
        let weak = ui.as_weak();
        ui.on_ime_key(move |text, down, ctrl, alt, shift, meta| {
            let mut model = model.borrow_mut();
            let modifier = model
                .pressed
                .values()
                .chain(model.latched.values())
                .any(|key| {
                    matches!(
                        key,
                        KeyCode::Scancode {
                            code: 0x1d | 0x38 | 0x5b,
                            ..
                        }
                    )
                });
            let special = matches!(key_code(&text), Some(KeyCode::Scancode { .. }))
                || ctrl
                || alt
                || meta
                || modifier
                || model.pressed.contains_key(text.as_str());
            if !special {
                return false;
            }
            if down {
                model.ime_previous.clear();
                if let Some(ui) = weak.upgrade() {
                    ui.set_ime_text("".into());
                }
            }
            remote_key(&mut model, &controller, &text, down, ctrl, alt, shift, meta)
        });
    }
    bind!(on_committed_text, |u, m, c, text| {
        let commands = if m.latched.is_empty() {
            committed_delta(&m.ime_previous, &text)
        } else {
            text.chars()
                .skip(m.ime_previous.chars().count())
                .flat_map(|ch| {
                    shortcut_key(&ch.to_string())
                        .map(|key| vec![SessionCommand::KeyDown(key), SessionCommand::KeyUp(key)])
                        .unwrap_or_default()
                })
                .collect()
        };
        send_batch(&u, c, commands);
        m.ime_previous = text.to_string();
    });
    bind!(on_special_key, |u, m, c, name| {
        let (code, extended) = match name.as_str() {
            "Escape" => (0x01, false),
            "Tab" => (0x0f, false),
            "Control" => (0x1d, false),
            "Alt" => (0x38, false),
            "Meta" => (0x5b, true),
            _ => (0x1c, false),
        };
        let key = KeyCode::Scancode { code, extended };
        if matches!(name.as_str(), "Control" | "Alt" | "Meta") {
            if m.latched.remove(name.as_str()).is_some() {
                result(&u, c.send(SessionCommand::KeyUp(key)), "修饰键已释放");
            } else if c.send(SessionCommand::KeyDown(key)).is_ok() {
                m.latched.insert(name.to_string(), key);
                feedback(&u, format!("{name} 已按下"), false);
            }
        } else {
            send_batch(
                &u,
                c,
                vec![SessionCommand::KeyDown(key), SessionCommand::KeyUp(key)],
            );
        }
        defer_focus(&u, true);
    });
    bind!(on_send_clipboard, |u, _m, c| {
        result(
            &u,
            c.send(SessionCommand::SetLocalClipboard(
                u.get_clipboard().to_string(),
            )),
            "剪贴板已提交给连接",
        );
    });
    bind!(on_request_clipboard, |u, _m, c| {
        result(
            &u,
            c.send(SessionCommand::RequestRemoteClipboard),
            "正在请求远端剪贴板…",
        );
    });
    bind!(on_read_local_clipboard, |u, _m, _c| {
        match NativePlatform.read_clipboard() {
            Ok(text) => u.set_clipboard(text.into()),
            Err(e) => feedback(&u, e, true),
        }
    });
    bind!(on_copy_remote_clipboard, |u, _m, _c| {
        result(
            &u,
            NativePlatform.write_clipboard(&u.get_clipboard()),
            "已复制到本机",
        );
    });

    let timer = Timer::default();
    {
        let weak = ui.as_weak();
        let model = model.clone();
        let controller = controller.clone();
        timer.start(TimerMode::Repeated, Duration::from_millis(33), move || {
            let Some(ui) = weak.upgrade() else {
                return;
            };
            let mut model = model.borrow_mut();
            if controller.0.lock().unwrap().suspended {
                model.pressed.clear();
                model.latched.clear();
                model.gestures.cancel();
            }
            let (generation, events) = controller.events();
            for event in events {
                if !controller.current(generation) {
                    break;
                }
                match event {
                    SessionEvent::CertificateTrustRequired { fingerprint } => {
                        let mut active = controller.0.lock().unwrap();
                        active.pending_fingerprint = Some(fingerprint.clone());
                        if let Some((profile, _)) = &active.credentials {
                            ui.set_certificate_target(profile.endpoint().into());
                            ui.set_certificate_changed(model.repository.as_ref().is_some_and(
                                |r| r.catalog.trust.contains_key(&endpoint_key(profile)),
                            ));
                        }
                        ui.set_pending_fingerprint(fingerprint.into());
                    }
                    SessionEvent::StateChanged(SessionState::Connected)
                    | SessionEvent::Connected { .. } => {
                        controller.0.lock().unwrap().connected = true;
                        ui.set_connected(true);
                        feedback(&ui, "已连接", false);
                        ui.set_pending_fingerprint("".into());
                        let width = model.gestures.viewport.width;
                        let height = model.gestures.viewport.height;
                        if model.dynamic_resolution && width > 1. && height > 1. {
                            let scale = ui.window().scale_factor();
                            let size = (
                                (width * scale).clamp(320., 8192.) as u16,
                                (height * scale).clamp(200., 8192.) as u16,
                            );
                            if model.last_size != Some(size) {
                                model.resize = Some((
                                    Instant::now() + Duration::from_millis(250),
                                    size.0,
                                    size.1,
                                ));
                            }
                        }
                    }
                    SessionEvent::StateChanged(SessionState::Connecting) => {
                        feedback(&ui, "正在连接…", false)
                    }
                    SessionEvent::Reconnecting {
                        attempt,
                        maximum_attempts,
                    } => {
                        release(&mut model, &controller);
                        controller.0.lock().unwrap().connected = false;
                        ui.set_connected(false);
                        feedback(&ui, format!("正在重连 {attempt}/{maximum_attempts}"), false);
                    }
                    SessionEvent::ClipboardText(text) => {
                        ui.set_clipboard(text.into());
                        feedback(&ui, "已收到远端剪贴板，可选择存入本机", false);
                    }
                    SessionEvent::Error(error) => feedback(&ui, format!("连接错误：{error}"), true),
                    SessionEvent::Disconnected { reason } => {
                        controller.finish(generation);
                        ui.set_connected(false);
                        ui.set_remote_image(Image::default());
                        model.pressed.clear();
                        model.latched.clear();
                        model.gestures.cancel();
                        feedback(&ui, format!("连接结束：{reason:?}"), true);
                    }
                    _ => {}
                }
            }
            if let Some(frame) = controller.frame() {
                if model.last_frame != (generation, frame.sequence) {
                    let pixels = SharedPixelBuffer::<Rgba8Pixel>::clone_from_slice(
                        frame.buffer.as_bytes(),
                        frame.width,
                        frame.height,
                    );
                    ui.set_remote_image(Image::from_rgba8(pixels));
                    model.gestures.viewport.remote_width = frame.width as f32;
                    model.gestures.viewport.remote_height = frame.height as f32;
                    model.gestures.viewport.clamp_pan();
                    update_view(&ui, &model);
                    model.last_frame = (generation, frame.sequence);
                }
            }
            if let Some((due, width, height)) = model.resize {
                if Instant::now() >= due && ui.get_connected() {
                    if controller
                        .send(SessionCommand::Resize {
                            width,
                            height,
                            scale_factor: model.desktop_scale,
                        })
                        .is_ok()
                    {
                        model.last_size = Some((width, height));
                    }
                    model.resize = None;
                }
            }
        });
    }
    let close_controller = controller.clone();
    ui.window().on_close_requested(move || {
        close_controller.disconnect();
        slint::CloseRequestResponse::HideWindow
    });
    Ok((ui, timer))
}
fn create_app(controller: Controller) -> Result<(), slint::PlatformError> {
    let (ui, timer) = prepare_app(controller.clone(), DeviceRepository::open(&NativePlatform))?;
    let outcome = ui.run();
    timer.stop();
    controller.disconnect();
    outcome
}
pub fn run() -> Result<(), slint::PlatformError> {
    create_app(Controller::default())
}

#[cfg(target_os = "android")]
#[unsafe(no_mangle)]
pub fn android_main(app: slint::android::AndroidApp) {
    use slint::android::android_activity::{MainEvent, PollEvent};
    let _activity_scope = unsafe { platform::bind_android_activity(app.activity_as_ptr()) }
        .expect("Android Activity binding failed");
    let controller = Controller::default();
    let lifecycle = controller.clone();
    slint::android::init_with_event_listener(app, move |event| match event {
        PollEvent::Main(MainEvent::Pause | MainEvent::LostFocus) => lifecycle.suspend(true),
        PollEvent::Main(MainEvent::Resume { .. } | MainEvent::GainedFocus) => {
            lifecycle.suspend(false)
        }
        PollEvent::Main(MainEvent::Destroy) => lifecycle.disconnect(),
        _ => {}
    })
    .expect("Android UI initialization failed");
    create_app(controller).expect("RemoteAPP UI failed");
}

#[cfg(test)]
mod ui_tests;
