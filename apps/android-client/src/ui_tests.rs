use super::*;
use slint::platform::{
    Platform, WindowAdapter,
    software_renderer::{MinimalSoftwareWindow, RepaintBufferType},
};
struct Headless(Rc<MinimalSoftwareWindow>);
impl Platform for Headless {
    fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, slint::PlatformError> {
        Ok(self.0.clone())
    }
}
#[test]
fn render_mobile_and_tablet_layouts() {
    let window = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
    slint::platform::set_platform(Box::new(Headless(window.clone()))).unwrap();
    let ui = MainWindow::new().unwrap();
    ui.set_storage_ready(true);
    ui.set_devices(ModelRc::new(VecModel::from(vec![
        DeviceRow {
            id: "one".into(),
            name: "办公室电脑".into(),
            endpoint: "192.168.1.12:3389".into(),
            username: "alice".into(),
            favorite: true,
        },
        DeviceRow {
            id: "two".into(),
            name: "工作站".into(),
            endpoint: "workstation.local:3389".into(),
            username: "developer".into(),
            favorite: false,
        },
    ])));
    ui.show().unwrap();
    let out = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/ui-review");
    std::fs::create_dir_all(&out).unwrap();
    for (name, width, height, page) in [
        ("phone-home", 390, 844, 0),
        ("phone-edit", 390, 844, 1),
        ("landscape-edit", 844, 390, 1),
        ("tablet-home", 1024, 768, 0),
        ("phone-session", 390, 844, 2),
        ("tablet-session", 1024, 768, 2),
        ("phone-settings", 390, 844, 3),
    ] {
        ui.set_page(page);
        ui.set_session_name("办公室电脑".into());
        ui.set_connected(true);
        ui.set_status(if page == 2 {
            "已连接".into()
        } else {
            "".into()
        });
        window.set_size(slint::PhysicalSize::new(width, height));
        window.dispatch_event(slint::platform::WindowEvent::Resized {
            size: slint::LogicalSize::new(width as f32, height as f32),
        });
        slint::platform::update_timers_and_animations();
        window.request_redraw();
        let mut pixels = vec![slint::Rgb8Pixel::default(); (width * height) as usize];
        assert!(window.draw_if_needed(|renderer| {
            renderer.render(&mut pixels, width as usize);
        }));
        let bytes: Vec<u8> = pixels.into_iter().flat_map(|p| [p.r, p.g, p.b]).collect();
        image::save_buffer(
            out.join(format!("{name}.png")),
            &bytes,
            width,
            height,
            image::ColorType::Rgb8,
        )
        .unwrap();
    }
    ui.hide().unwrap();

    // Exercise actual installed callbacks, not only the UI component in isolation.
    drop(ui);
    let directory = std::env::temp_dir().join(format!("remoteapp-ui-flow-{}", ProfileId::new()));
    std::fs::create_dir_all(&directory).unwrap();
    let repository = DeviceRepository::for_test(directory.clone());
    let controller = Controller::default();
    let (app, timer) = prepare_app(controller.clone(), Ok(repository)).unwrap();
    app.show().unwrap();
    app.invoke_new_device();
    assert_eq!(app.get_page(), 1);
    app.set_device_name("测试电脑".into());
    app.set_host("127.0.0.1".into());
    app.set_username("alice".into());
    app.set_password("temporary".into());
    app.invoke_save_device(false);
    assert!(!app.get_error());
    assert_eq!(app.get_page(), 0);
    assert!(app.get_password().is_empty());
    use slint::Model;
    let row = app.get_devices().row_data(0).unwrap();
    app.invoke_toggle_favorite(row.id.clone());
    assert!(app.get_devices().row_data(0).unwrap().favorite);
    app.invoke_connect_device(row.id.clone());
    assert_eq!(app.get_page(), 1); // No password was saved; never start a real network connection.
    assert!(app.get_password().is_empty());
    let (commands, mut received) = tokio::sync::mpsc::unbounded_channel();
    let (_events_sender, events) = tokio::sync::mpsc::unbounded_channel();
    let (_, frames) = tokio::sync::watch::channel(None);
    controller.begin(
        ConnectionProfile::default(),
        Secret::new("test"),
        remoteapp_rdp_core::SessionHandle {
            commands,
            events,
            frames,
        },
    );
    controller.0.lock().unwrap().connected = true;
    app.set_connected(true);
    app.set_page(2);
    let control = char::from(slint::platform::Key::Control).to_string();
    assert!(app.invoke_key(control.clone().into(), true, false, false, false, false));
    assert!(app.invoke_key("c".into(), true, false, false, false, false));
    let mut sent = Vec::new();
    while let Ok(command) = received.try_recv() {
        sent.push(command);
    }
    assert!(sent.iter().any(|c| matches!(
        c,
        SessionCommand::KeyDown(KeyCode::Scancode { code: 0x2e, .. })
    )));
    app.invoke_disconnect();
    app.invoke_disconnect();
    assert!(!app.get_connected());
    assert!(controller.0.lock().unwrap().credentials.is_none());
    app.invoke_delete_device(row.id);
    assert_eq!(app.get_devices().row_count(), 0);
    timer.stop();
    app.hide().unwrap();
    std::fs::remove_dir_all(directory).unwrap();
}
