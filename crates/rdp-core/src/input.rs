use std::collections::VecDeque;

use super::model::DesktopConfig;

pub const INPUT_QUEUE_CAPACITY: usize = 128;

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum MouseButton {
    Left,
    Middle,
    Right,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum KeyCode {
    Scancode { code: u8, extended: bool },
    Unicode(char),
}

#[derive(Debug, Clone, PartialEq)]
pub enum InputOperation {
    PointerMove { x: u16, y: u16 },
    ButtonDown(MouseButton),
    ButtonUp(MouseButton),
    Wheel { vertical: bool, units: i16 },
    KeyDown(KeyCode),
    KeyUp(KeyCode),
    ClipboardText(String),
    RequestRemoteClipboard,
    Resize(DesktopConfig),
    Disconnect,
}

#[derive(Debug, Default)]
pub struct InputQueue {
    queue: VecDeque<InputOperation>,
}

impl InputQueue {
    #[must_use]
    pub fn len(&self) -> usize {
        self.queue.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.queue.is_empty()
    }

    pub fn push(&mut self, operation: InputOperation) -> Result<(), InputOperation> {
        if let Some(InputOperation::PointerMove { x, y }) = self.queue.back_mut()
            && let InputOperation::PointerMove {
                x: next_x,
                y: next_y,
            } = operation
        {
            *x = next_x;
            *y = next_y;
            return Ok(());
        }
        if self.queue.len() >= INPUT_QUEUE_CAPACITY {
            if let Some(index) = self
                .queue
                .iter()
                .position(|entry| matches!(entry, InputOperation::PointerMove { .. }))
            {
                self.queue.remove(index);
            } else {
                return Err(operation);
            }
        }
        self.queue.push_back(operation);
        Ok(())
    }

    pub fn pop(&mut self) -> Option<InputOperation> {
        self.queue.pop_front()
    }
}

#[derive(Debug)]
pub struct TouchpadMapper {
    remote_width: u16,
    remote_height: u16,
    cursor_x: i32,
    cursor_y: i32,
    pressed: bool,
}

impl TouchpadMapper {
    #[must_use]
    pub fn new(remote_width: u16, remote_height: u16) -> Self {
        Self {
            remote_width: remote_width.max(1),
            remote_height: remote_height.max(1),
            cursor_x: i32::from(remote_width / 2),
            cursor_y: i32::from(remote_height / 2),
            pressed: false,
        }
    }

    pub fn update_remote_size(&mut self, width: u16, height: u16) {
        self.remote_width = width.max(1);
        self.remote_height = height.max(1);
        self.cursor_x = self.cursor_x.clamp(0, i32::from(self.remote_width - 1));
        self.cursor_y = self.cursor_y.clamp(0, i32::from(self.remote_height - 1));
    }

    #[must_use]
    pub fn move_by(&mut self, dx: f32, dy: f32) -> InputOperation {
        self.cursor_x =
            (self.cursor_x + dx.round() as i32).clamp(0, i32::from(self.remote_width - 1));
        self.cursor_y =
            (self.cursor_y + dy.round() as i32).clamp(0, i32::from(self.remote_height - 1));
        InputOperation::PointerMove {
            x: self.cursor_x as u16,
            y: self.cursor_y as u16,
        }
    }

    #[must_use]
    pub fn tap(&mut self) -> [InputOperation; 2] {
        [
            InputOperation::ButtonDown(MouseButton::Left),
            InputOperation::ButtonUp(MouseButton::Left),
        ]
    }

    #[must_use]
    pub fn long_press(&mut self) -> [InputOperation; 2] {
        [
            InputOperation::ButtonDown(MouseButton::Right),
            InputOperation::ButtonUp(MouseButton::Right),
        ]
    }

    #[must_use]
    pub fn begin_drag(&mut self) -> InputOperation {
        self.pressed = true;
        InputOperation::ButtonDown(MouseButton::Left)
    }

    #[must_use]
    pub fn end_drag(&mut self) -> Option<InputOperation> {
        self.pressed
            .then_some(InputOperation::ButtonUp(MouseButton::Left))
            .inspect(|_| self.pressed = false)
    }

    #[must_use]
    pub fn scroll(&self, units: i16) -> InputOperation {
        InputOperation::Wheel {
            vertical: true,
            units,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn queue_coalesces_pointer_moves() {
        let mut queue = InputQueue::default();
        queue
            .push(InputOperation::PointerMove { x: 1, y: 2 })
            .unwrap();
        queue
            .push(InputOperation::PointerMove { x: 3, y: 4 })
            .unwrap();
        assert_eq!(queue.len(), 1);
        assert_eq!(
            queue.pop(),
            Some(InputOperation::PointerMove { x: 3, y: 4 })
        );
    }

    #[test]
    fn touchpad_clamps_cursor_to_remote_desktop() {
        let mut mapper = TouchpadMapper::new(100, 80);
        assert_eq!(
            mapper.move_by(-1000.0, -1000.0),
            InputOperation::PointerMove { x: 0, y: 0 }
        );
        assert_eq!(
            mapper.move_by(1000.0, 1000.0),
            InputOperation::PointerMove { x: 99, y: 79 }
        );
    }
}
