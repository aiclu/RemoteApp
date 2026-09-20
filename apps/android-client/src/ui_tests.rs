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
    let connections = Rc::new(std::cell::Cell::new(0));
    let count = connections.clone();
    ui.on_connect_device(move |_| count.set(count.get() + 1));
    let viewport = Rc::new(std::cell::Cell::new((0_f32, 0_f32)));
    let observed = viewport.clone();
    ui.on_viewport(move |width, height| observed.set((width, height)));
    let saved_devices = ui.get_devices();
    ui.show().unwrap();
    let out = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/ui-review");
    std::fs::create_dir_all(&out).unwrap();
    for (name, width, height, page) in [
        ("phone-home", 390, 844, 0),
        ("phone-home-dark", 390, 844, 0),
        ("narrow-home", 320, 640, 0),
        ("phone-more", 390, 844, 0),
        ("phone-delete", 390, 844, 0),
        ("phone-certificate", 390, 844, 2),
        ("landscape-certificate", 844, 390, 2),
        ("phone-long-name", 390, 844, 0),
        ("phone-favorites", 390, 844, 0),
        ("phone-empty", 390, 844, 0),
        ("phone-empty-storage-error", 390, 844, 0),
        ("phone-edit", 390, 844, 1),
        ("landscape-edit", 844, 390, 1),
        ("tablet-home", 1024, 768, 0),
        ("phone-session", 390, 844, 2),
        ("phone-session-menu", 390, 844, 2),
        ("landscape-session-menu", 844, 390, 2),
        ("phone-session-keyboard", 390, 844, 2),
        ("tablet-session", 1024, 768, 2),
        ("phone-settings", 390, 844, 3),
        ("tablet-settings-dark", 1024, 768, 3),
        ("phone-edit-keyboard", 390, 440, 1),
    ] {
        ui.set_devices(if name.contains("empty") {
            ModelRc::default()
        } else {
            saved_devices.clone()
        });
        if name.contains("long-name") {
            ui.set_devices(ModelRc::new(VecModel::from(vec![DeviceRow {
                id: "long".into(),
                name: "用于验证超长名称的研发办公室电脑与工作站".into(),
                endpoint: "workstation.intranet.example.com:45988".into(),
                username: "CORPORATE\\long-username".into(),
                favorite: true,
            }])));
        }
        ui.set_dark(name.contains("dark"));
        ui.set_favorites_only(name.contains("favorites"));
        ui.set_error(name.contains("storage-error"));
        ui.set_storage_ready(!name.contains("storage-error"));
        ui.set_session_menu_visible(false);
        ui.set_keyboard_visible(name.contains("keyboard"));
        ui.set_page(page);
        ui.set_menu_name("办公室电脑".into());
        ui.set_menu_id(if name == "phone-more" {
            "one".into()
        } else {
            "".into()
        });
        ui.set_delete_id(if name == "phone-delete" {
            "one".into()
        } else {
            "".into()
        });
        ui.set_pending_fingerprint(if name.contains("certificate") { "AA:BB:CC:DD:EE:FF:00:11:22:33:44:55:66:77:88:99:AA:BB:CC:DD:EE:FF:00:11:22:33:44:55:66:77:88:99".into() } else { "".into() });
        ui.set_certificate_target("workstation.example.com:3389".into());
        ui.set_certificate_changed(true);
        ui.set_session_name("办公室电脑".into());
        ui.set_connected(true);
        ui.set_status(if name.contains("storage-error") {
            "安全存储不可用：测试错误信息".into()
        } else if page == 2 {
            "已连接".into()
        } else {
            "".into()
        });
        window.set_size(slint::PhysicalSize::new(width, height));
        window.dispatch_event(slint::platform::WindowEvent::Resized {
            size: slint::LogicalSize::new(width as f32, height as f32),
        });
        slint::platform::update_timers_and_animations();
        let before_menu = viewport.get();
        ui.set_session_menu_visible(name.contains("menu"));
        slint::platform::update_timers_and_animations();
        if page == 2 && name.contains("menu") {
            assert_eq!(
                viewport.get(),
                before_menu,
                "Floating menu must not resize the desktop"
            );
        }
        window.request_redraw();
        let mut pixels = vec![slint::Rgb8Pixel::default(); (width * height) as usize];
        assert!(window.draw_if_needed(|renderer| {
            renderer.render(&mut pixels, width as usize);
        }));
        if page == 2 && !name.contains("keyboard") {
            let (canvas_width, canvas_height) = viewport.get();
            assert!(
                (canvas_width - width as f32).abs() < 1.,
                "Session canvas must fill available width"
            );
            assert!(
                (canvas_height - height as f32).abs() < 1.,
                "Floating controls must not reserve vertical space"
            );
        }
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
    ui.set_page(0);
    ui.set_dark(false);
    ui.set_menu_id("".into());
    ui.set_pending_fingerprint("".into());
    ui.set_delete_id("".into());
    ui.set_devices(saved_devices);
    window.set_size(slint::PhysicalSize::new(390, 844));
    window.dispatch_event(slint::platform::WindowEvent::Resized {
        size: slint::LogicalSize::new(390., 844.),
    });
    slint::platform::update_timers_and_animations();
    let mut hit_pixels = vec![slint::Rgb8Pixel::default(); 390 * 844];
    window.request_redraw();
    window.draw_if_needed(|renderer| {
        renderer.render(&mut hit_pixels, 390);
    });
    let click = |x, y| {
        let position = slint::LogicalPosition::new(x, y);
        window.dispatch_event(slint::platform::WindowEvent::PointerPressed {
            position,
            button: slint::platform::PointerEventButton::Left,
        });
        window.dispatch_event(slint::platform::WindowEvent::PointerReleased {
            position,
            button: slint::platform::PointerEventButton::Left,
        });
        slint::platform::update_timers_and_animations();
    };
    click(338., 220.);
    assert_eq!(
        ui.get_menu_id(),
        "one",
        "More button opens the correct device menu"
    );
    assert_eq!(
        connections.get(),
        0,
        "More button must not connect the device"
    );
    ui.set_menu_id("".into());
    click(110., 220.);
    assert_eq!(connections.get(), 1, "Clicking the card connects once");
    click(195., 810.);
    assert!(ui.get_favorites_only());
    click(324., 810.);
    assert_eq!(ui.get_page(), 3);
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

#[test]
fn display_modes_preserve_landscape_and_follow_physical_pixels() {
    assert_eq!(landscape_size(1080, 1920), (1920, 1080));
    assert_eq!(
        display_size(false, (1920, 1080), 390., 844., 3.),
        (1920, 1080)
    );
    assert_eq!(
        display_size(false, (1920, 1080), 844., 390., 3.),
        (1920, 1080)
    );
    assert_eq!(
        display_size(true, (1920, 1080), 390., 844., 3.),
        (1170, 2532)
    );
    assert_eq!(
        display_size(true, (1920, 1080), 844., 390., 3.),
        (2532, 1170)
    );
}
