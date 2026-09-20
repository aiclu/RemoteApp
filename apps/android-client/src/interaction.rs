use remoteapp_rdp_core::{KeyCode, MouseButton, SessionCommand};
use std::{
    collections::BTreeMap,
    time::{Duration, Instant},
};

#[derive(Clone, Copy, Debug)]
pub struct Viewport {
    pub width: f32,
    pub height: f32,
    pub remote_width: f32,
    pub remote_height: f32,
    pub zoom: f32,
    pub pan_x: f32,
    pub pan_y: f32,
}
impl Default for Viewport {
    fn default() -> Self {
        Self {
            width: 1.,
            height: 1.,
            remote_width: 1920.,
            remote_height: 1080.,
            zoom: 1.,
            pan_x: 0.,
            pan_y: 0.,
        }
    }
}
impl Viewport {
    pub fn scale(&self) -> f32 {
        (self.width / self.remote_width)
            .min(self.height / self.remote_height)
            .max(0.0001)
            * self.zoom
    }
    pub fn origin(&self) -> (f32, f32) {
        (
            (self.width - self.remote_width * self.scale()) / 2. + self.pan_x,
            (self.height - self.remote_height * self.scale()) / 2. + self.pan_y,
        )
    }
    pub fn map(&self, x: f32, y: f32) -> Option<(u16, u16)> {
        let (left, top) = self.origin();
        let x = (x - left) / self.scale();
        let y = (y - top) / self.scale();
        if x < 0. || y < 0. || x >= self.remote_width || y >= self.remote_height {
            return None;
        }
        Some((x.floor() as u16, y.floor() as u16))
    }
    pub fn clamp_pan(&mut self) {
        let max_x = ((self.remote_width * self.scale() - self.width) / 2.).max(0.);
        let max_y = ((self.remote_height * self.scale() - self.height) / 2.).max(0.);
        self.pan_x = self.pan_x.clamp(-max_x, max_x);
        self.pan_y = self.pan_y.clamp(-max_y, max_y);
    }
}
struct Finger {
    start: (f32, f32),
    last: (f32, f32),
    down: Instant,
    moved: bool,
}
pub struct Gestures {
    pub viewport: Viewport,
    pub direct: bool,
    pub pan_mode: bool,
    cursor: (f32, f32),
    fingers: BTreeMap<i32, Finger>,
    last_tap: Option<(Instant, (f32, f32))>,
    dragging: bool,
    multi: bool,
    pinching: bool,
    pair_start: f32,
}
impl Default for Gestures {
    fn default() -> Self {
        Self {
            viewport: Viewport::default(),
            direct: false,
            pan_mode: false,
            cursor: (960., 540.),
            fingers: BTreeMap::new(),
            last_tap: None,
            dragging: false,
            multi: false,
            pinching: false,
            pair_start: 0.,
        }
    }
}
impl Gestures {
    pub fn cancel(&mut self) -> Vec<SessionCommand> {
        self.fingers.clear();
        self.dragging = false;
        self.multi = false;
        self.last_tap = None;
        vec![SessionCommand::ReleaseAll]
    }
    pub fn reset(&mut self) {
        *self = Self::default();
    }
    pub fn click(button: MouseButton) -> Vec<SessionCommand> {
        vec![
            SessionCommand::ButtonDown(button),
            SessionCommand::ButtonUp(button),
        ]
    }
    fn pair(&self) -> Option<((f32, f32), f32)> {
        let mut values = self.fingers.values();
        let a = values.next()?.last;
        let b = values.next()?.last;
        Some((
            ((a.0 + b.0) / 2., (a.1 + b.1) / 2.),
            (a.0 - b.0).hypot(a.1 - b.1).max(1.),
        ))
    }
    /// kind: down=0, move=1, up=2, cancel=3. Finger zero denotes a physical mouse.
    pub fn event(
        &mut self,
        id: i32,
        kind: i32,
        x: f32,
        y: f32,
        button: i32,
        now: Instant,
    ) -> Vec<SessionCommand> {
        if kind == 3 {
            return self.cancel();
        }
        let mut output = Vec::new();
        if id == 0 {
            if let Some((x, y)) = self.viewport.map(x, y) {
                output.push(SessionCommand::PointerMove { x, y });
            }
            let button = if button == 1 {
                MouseButton::Right
            } else if button == 2 {
                MouseButton::Middle
            } else {
                MouseButton::Left
            };
            if kind == 0 {
                output.push(SessionCommand::ButtonDown(button));
            }
            if kind == 2 {
                output.push(SessionCommand::ButtonUp(button));
            }
            return output;
        }
        if kind == 0 {
            let double = self.last_tap.is_some_and(|(t, p)| {
                now.duration_since(t) < Duration::from_millis(350) && (x - p.0).hypot(y - p.1) < 24.
            });
            if self.fingers.is_empty() {
                self.multi = false;
                self.pinching = false;
            }
            self.fingers.insert(
                id,
                Finger {
                    start: (x, y),
                    last: (x, y),
                    down: now,
                    moved: false,
                },
            );
            if self.fingers.len() == 1 && double && !self.pan_mode {
                if self.direct {
                    if let Some((x, y)) = self.viewport.map(x, y) {
                        output.push(SessionCommand::PointerMove { x, y });
                    }
                }
                output.push(SessionCommand::ButtonDown(MouseButton::Left));
                self.dragging = true;
            } else if self.fingers.len() > 1 {
                self.multi = true;
                self.last_tap = None;
                if self.dragging {
                    output.push(SessionCommand::ButtonUp(MouseButton::Left));
                    self.dragging = false;
                }
                self.pair_start = self.pair().map_or(1., |p| p.1);
            }
            return output;
        }
        if kind == 1 {
            let previous_pair = self.pair();
            let Some(finger) = self.fingers.get_mut(&id) else {
                return output;
            };
            let (dx, dy) = (x - finger.last.0, y - finger.last.1);
            finger.last = (x, y);
            finger.moved |= (x - finger.start.0).hypot(y - finger.start.1) > 8.;
            if self.fingers.len() >= 2 {
                if let (Some((old_center, old_distance)), Some((center, distance))) =
                    (previous_pair, self.pair())
                {
                    self.pinching |= (distance - self.pair_start).abs() > 14.;
                    if self.pinching {
                        let old_scale = self.viewport.scale();
                        let old_origin = self.viewport.origin();
                        let point = (
                            (old_center.0 - old_origin.0) / old_scale,
                            (old_center.1 - old_origin.1) / old_scale,
                        );
                        self.viewport.zoom =
                            (self.viewport.zoom * distance / old_distance).clamp(1., 4.);
                        let scale = self.viewport.scale();
                        self.viewport.pan_x = center.0
                            - point.0 * scale
                            - (self.viewport.width - self.viewport.remote_width * scale) / 2.;
                        self.viewport.pan_y = center.1
                            - point.1 * scale
                            - (self.viewport.height - self.viewport.remote_height * scale) / 2.;
                        self.viewport.clamp_pan();
                    } else if self.pan_mode {
                        self.viewport.pan_x += center.0 - old_center.0;
                        self.viewport.pan_y += center.1 - old_center.1;
                        self.viewport.clamp_pan();
                    } else {
                        let units = ((old_center.1 - center.1) * 6.).round() as i16;
                        if units != 0 {
                            output.push(SessionCommand::Wheel {
                                vertical: true,
                                units,
                            });
                        }
                    }
                }
            } else if !self.multi {
                if self.pan_mode {
                    self.viewport.pan_x += dx;
                    self.viewport.pan_y += dy;
                    self.viewport.clamp_pan();
                } else if self.direct {
                    if self.dragging {
                        if let Some((x, y)) = self.viewport.map(x, y) {
                            output.push(SessionCommand::PointerMove { x, y });
                        }
                    }
                } else {
                    self.cursor.0 = (self.cursor.0 + dx / self.viewport.scale())
                        .clamp(0., self.viewport.remote_width - 1.);
                    self.cursor.1 = (self.cursor.1 + dy / self.viewport.scale())
                        .clamp(0., self.viewport.remote_height - 1.);
                    output.push(SessionCommand::PointerMove {
                        x: self.cursor.0 as u16,
                        y: self.cursor.1 as u16,
                    });
                }
            }
            return output;
        }
        if kind == 2 {
            let Some(finger) = self.fingers.remove(&id) else {
                return output;
            };
            if self.dragging {
                output.push(SessionCommand::ButtonUp(MouseButton::Left));
                self.dragging = false;
                self.last_tap = None;
            } else if !self.multi && !finger.moved && !self.pan_mode {
                if self.direct {
                    let Some((x, y)) = self.viewport.map(x, y) else {
                        return output;
                    };
                    output.push(SessionCommand::PointerMove { x, y });
                }
                let right = now.duration_since(finger.down) >= Duration::from_millis(500);
                output.extend(Self::click(if right {
                    MouseButton::Right
                } else {
                    MouseButton::Left
                }));
                self.last_tap = if right { None } else { Some((now, (x, y))) };
            }
        }
        output
    }
}
pub fn key_code(text: &str) -> Option<KeyCode> {
    let ch = text.chars().next()?;
    use slint::platform::Key;
    if (char::from(Key::F1)..=char::from(Key::F12)).contains(&ch) {
        let index = ch as u32 - char::from(Key::F1) as u32;
        return Some(KeyCode::Scancode {
            code: if index < 10 {
                0x3b + index as u8
            } else {
                0x57 + (index - 10) as u8
            },
            extended: false,
        });
    }
    let code = if ch == char::from(Key::Return) {
        0x1c
    } else if ch == char::from(Key::Tab) {
        0x0f
    } else if ch == char::from(Key::Backspace) {
        0x0e
    } else if ch == char::from(Key::Escape) {
        0x01
    } else if ch == char::from(Key::Control) {
        0x1d
    } else if ch == char::from(Key::Shift) {
        0x2a
    } else if ch == char::from(Key::Alt) {
        0x38
    } else if ch == char::from(Key::Meta) {
        return Some(KeyCode::Scancode {
            code: 0x5b,
            extended: true,
        });
    } else {
        let ext = if ch == char::from(Key::LeftArrow) {
            Some(0x4b)
        } else if ch == char::from(Key::RightArrow) {
            Some(0x4d)
        } else if ch == char::from(Key::UpArrow) {
            Some(0x48)
        } else if ch == char::from(Key::DownArrow) {
            Some(0x50)
        } else if ch == char::from(Key::Delete) {
            Some(0x53)
        } else if ch == char::from(Key::Home) {
            Some(0x47)
        } else if ch == char::from(Key::End) {
            Some(0x4f)
        } else if ch == char::from(Key::PageUp) {
            Some(0x49)
        } else if ch == char::from(Key::PageDown) {
            Some(0x51)
        } else {
            None
        };
        if let Some(code) = ext {
            return Some(KeyCode::Scancode {
                code,
                extended: true,
            });
        }
        if ch.is_control() || ('\u{e000}'..='\u{f8ff}').contains(&ch) {
            return None;
        }
        return Some(KeyCode::Unicode(ch));
    };
    Some(KeyCode::Scancode {
        code,
        extended: false,
    })
}
pub fn shortcut_key(text: &str) -> Option<KeyCode> {
    let ch = text.to_ascii_lowercase().chars().next()?;
    let code = match ch {
        'a' => 0x1e,
        'b' => 0x30,
        'c' => 0x2e,
        'd' => 0x20,
        'e' => 0x12,
        'f' => 0x21,
        'g' => 0x22,
        'h' => 0x23,
        'i' => 0x17,
        'j' => 0x24,
        'k' => 0x25,
        'l' => 0x26,
        'm' => 0x32,
        'n' => 0x31,
        'o' => 0x18,
        'p' => 0x19,
        'q' => 0x10,
        'r' => 0x13,
        's' => 0x1f,
        't' => 0x14,
        'u' => 0x16,
        'v' => 0x2f,
        'w' => 0x11,
        'x' => 0x2d,
        'y' => 0x15,
        'z' => 0x2c,
        _ => return key_code(text),
    };
    Some(KeyCode::Scancode {
        code,
        extended: false,
    })
}
/// TextInput exposes committed text separately from IME preedit.
pub fn committed_delta(previous: &str, next: &str) -> Vec<SessionCommand> {
    let common = previous
        .chars()
        .zip(next.chars())
        .take_while(|(a, b)| a == b)
        .count();
    let mut commands = Vec::new();
    for _ in common..previous.chars().count() {
        let key = KeyCode::Scancode {
            code: 0x0e,
            extended: false,
        };
        commands.extend([SessionCommand::KeyDown(key), SessionCommand::KeyUp(key)]);
    }
    for ch in next.chars().skip(common) {
        commands.extend([
            SessionCommand::KeyDown(KeyCode::Unicode(ch)),
            SessionCommand::KeyUp(KeyCode::Unicode(ch)),
        ]);
    }
    commands
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn maps_letterbox_zoom_and_pan() {
        let mut v = Viewport {
            width: 400.,
            height: 400.,
            remote_width: 1600.,
            remote_height: 900.,
            ..Default::default()
        };
        assert!(v.map(20., 20.).is_none());
        assert_eq!(v.map(200., 200.), Some((800, 450)));
        v.zoom = 2.;
        v.pan_x = 50.;
        assert_eq!(v.map(250., 200.), Some((800, 450)));
    }
    #[test]
    fn pointer_uses_delta_and_cancel_releases_drag() {
        let mut g = Gestures::default();
        g.viewport.width = 1920.;
        g.viewport.height = 1080.;
        let now = Instant::now();
        g.event(1, 0, 10., 10., 0, now);
        assert!(matches!(
            g.event(1, 1, 20., 10., 0, now)[0],
            SessionCommand::PointerMove { x: 970, y: 540 }
        ));
        g.event(1, 2, 20., 10., 0, now);
        g.event(1, 0, 300., 300., 0, now);
        assert!(matches!(
            g.event(1, 1, 310., 300., 0, now)[0],
            SessionCommand::PointerMove { x: 980, y: 540 }
        ));
        assert!(matches!(g.cancel()[0], SessionCommand::ReleaseAll));
    }
    #[test]
    fn committed_unicode_is_not_duplicated() {
        assert_eq!(committed_delta("", "中文").len(), 4);
        assert!(committed_delta("中文", "中文").is_empty());
        assert_eq!(committed_delta("中文", "中").len(), 2);
    }

    #[test]
    fn direct_touch_respects_remote_dimensions() {
        let mut g = Gestures::default();
        g.direct = true;
        g.viewport = Viewport {
            width: 400.,
            height: 300.,
            remote_width: 800.,
            remote_height: 600.,
            ..Default::default()
        };
        let now = Instant::now();
        g.event(1, 0, 100., 50., 0, now);
        assert!(matches!(
            g.event(1, 2, 100., 50., 0, now)[0],
            SessionCommand::PointerMove { x: 200, y: 100 }
        ));
    }
    #[test]
    fn long_press_and_double_tap_drag_are_distinct() {
        let mut g = Gestures::default();
        let now = Instant::now();
        g.event(1, 0, 10., 10., 0, now);
        let right = g.event(1, 2, 10., 10., 0, now + Duration::from_millis(600));
        assert!(matches!(
            right[0],
            SessionCommand::ButtonDown(MouseButton::Right)
        ));
        g.event(1, 0, 10., 10., 0, now + Duration::from_millis(800));
        g.event(1, 2, 10., 10., 0, now + Duration::from_millis(850));
        assert!(matches!(
            g.event(1, 0, 10., 10., 0, now + Duration::from_millis(950))[0],
            SessionCommand::ButtonDown(MouseButton::Left)
        ));
        assert!(matches!(
            g.event(1, 2, 40., 40., 0, now + Duration::from_millis(1000))[0],
            SessionCommand::ButtonUp(MouseButton::Left)
        ));
    }
    #[test]
    fn two_finger_scroll_does_not_click_and_pinch_is_local() {
        let mut g = Gestures::default();
        g.viewport.width = 400.;
        g.viewport.height = 300.;
        let now = Instant::now();
        g.event(1, 0, 100., 100., 0, now);
        g.event(2, 0, 200., 100., 0, now);
        assert!(
            g.event(1, 1, 100., 95., 0, now)
                .iter()
                .any(|c| matches!(c, SessionCommand::Wheel { .. }))
        );
        let pinch = g.event(2, 1, 270., 95., 0, now);
        assert!(pinch.is_empty());
        assert!(g.viewport.zoom > 1.);
        assert!(g.event(1, 2, 100., 95., 0, now).is_empty());
        assert!(g.event(2, 2, 270., 95., 0, now).is_empty());
    }
}
