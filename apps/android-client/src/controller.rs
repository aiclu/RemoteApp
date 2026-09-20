use remoteapp_rdp_core::{ConnectionProfile, FrameUpdate, Secret, SessionCommand, SessionHandle};
use std::sync::{Arc, Mutex};
use tokio::sync::{mpsc, watch};

#[derive(Default)]
pub struct Active {
    pub generation: u64,
    pub commands: Option<mpsc::UnboundedSender<SessionCommand>>,
    pub frames: Option<watch::Receiver<Option<Arc<FrameUpdate>>>>,
    pub credentials: Option<(ConnectionProfile, Secret)>,
    pub pending_fingerprint: Option<String>,
    pub events: Option<mpsc::UnboundedReceiver<remoteapp_rdp_core::SessionEvent>>,
    pub connected: bool,
    pub suspended: bool,
}
#[derive(Clone, Default)]
pub struct Controller(pub Arc<Mutex<Active>>);
impl Controller {
    pub fn begin(
        &self,
        profile: ConnectionProfile,
        password: Secret,
        handle: SessionHandle,
    ) -> u64 {
        let mut active = self.0.lock().unwrap();
        if let Some(previous) = active.commands.take() {
            let _ = previous.send(SessionCommand::ReleaseAll);
            let _ = previous.send(SessionCommand::Disconnect);
        }
        active.generation += 1;
        active.commands = Some(handle.commands.clone());
        active.frames = Some(handle.frames);
        active.events = Some(handle.events);
        active.credentials = Some((profile, password));
        active.pending_fingerprint = None;
        active.connected = false;
        active.generation
    }
    pub fn events(&self) -> (u64, Vec<remoteapp_rdp_core::SessionEvent>) {
        let mut active = self.0.lock().unwrap();
        let generation = active.generation;
        let mut output = Vec::new();
        if let Some(events) = &mut active.events {
            for _ in 0..64 {
                match events.try_recv() {
                    Ok(event) => output.push(event),
                    Err(_) => break,
                }
            }
        }
        (generation, output)
    }
    pub fn current(&self, generation: u64) -> bool {
        self.0.lock().unwrap().generation == generation
    }
    pub fn send(&self, command: SessionCommand) -> Result<(), String> {
        let active = self.0.lock().unwrap();
        if !active.connected {
            return Err("当前没有已连接的会话".into());
        }
        active
            .commands
            .as_ref()
            .ok_or("会话已结束")?
            .send(command)
            .map_err(|_| "连接已断开".into())
    }
    pub fn release_inputs(&self) {
        if let Some(commands) = &self.0.lock().unwrap().commands {
            let _ = commands.send(SessionCommand::ReleaseAll);
        }
    }
    pub fn disconnect(&self) {
        let mut active = self.0.lock().unwrap();
        active.generation += 1;
        if let Some(commands) = active.commands.take() {
            let _ = commands.send(SessionCommand::ReleaseAll);
            let _ = commands.send(SessionCommand::Disconnect);
        }
        active.frames = None;
        active.events = None;
        active.credentials = None;
        active.pending_fingerprint = None;
        active.connected = false;
    }
    pub fn finish(&self, generation: u64) {
        let mut active = self.0.lock().unwrap();
        if active.generation != generation {
            return;
        }
        active.commands = None;
        active.frames = None;
        active.events = None;
        active.connected = false;
        if active.pending_fingerprint.is_none() {
            active.credentials = None;
        }
    }
    #[cfg_attr(not(target_os = "android"), allow(dead_code))]
    pub fn suspend(&self, suspended: bool) {
        let mut active = self.0.lock().unwrap();
        active.suspended = suspended;
        if let Some(commands) = &active.commands {
            if suspended {
                let _ = commands.send(SessionCommand::ReleaseAll);
            }
            let _ = commands.send(if suspended {
                SessionCommand::SuspendRendering
            } else {
                SessionCommand::ResumeRendering
            });
        }
    }
    pub fn frame(&self) -> Option<Arc<FrameUpdate>> {
        let active = self.0.lock().unwrap();
        if !active.connected || active.suspended {
            return None;
        }
        active.frames.as_ref()?.borrow().clone()
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn handle() -> SessionHandle {
        let (commands, _) = mpsc::unbounded_channel();
        let (_, events) = mpsc::unbounded_channel();
        let (_, frames) = watch::channel(None);
        SessionHandle {
            commands,
            events,
            frames,
        }
    }
    #[test]
    fn stale_session_cannot_clear_new_connection() {
        let c = Controller::default();
        let first = c.begin(ConnectionProfile::default(), Secret::new("a"), handle());
        let second = c.begin(ConnectionProfile::default(), Secret::new("b"), handle());
        c.finish(first);
        assert!(!c.current(first));
        assert!(c.current(second));
        assert!(c.0.lock().unwrap().commands.is_some());
        c.disconnect();
        c.disconnect();
        assert!(!c.current(second));
        assert!(c.0.lock().unwrap().credentials.is_none());
    }
    #[test]
    fn disconnected_commands_fail() {
        assert!(
            Controller::default()
                .send(SessionCommand::RequestRemoteClipboard)
                .is_err()
        );
    }

    #[test]
    fn disconnect_discards_pending_certificate_events() {
        let c = Controller::default();
        let (commands, _) = mpsc::unbounded_channel();
        let (sender, events) = mpsc::unbounded_channel();
        let (_, frames) = watch::channel(None);
        c.begin(
            ConnectionProfile::default(),
            Secret::new("private"),
            SessionHandle {
                commands,
                events,
                frames,
            },
        );
        sender
            .send(remoteapp_rdp_core::SessionEvent::CertificateTrustRequired {
                fingerprint: "old".into(),
            })
            .unwrap();
        c.disconnect();
        assert!(c.events().1.is_empty());
        assert!(c.0.lock().unwrap().credentials.is_none());
    }
    #[test]
    fn only_latest_frame_survives_and_suspend_hides_it() {
        let c = Controller::default();
        let (commands, _) = mpsc::unbounded_channel();
        let (_, events) = mpsc::unbounded_channel();
        let (sender, frames) = watch::channel(None);
        c.begin(
            ConnectionProfile::default(),
            Secret::new("p"),
            SessionHandle {
                commands,
                events,
                frames,
            },
        );
        c.0.lock().unwrap().connected = true;
        for sequence in 1..101 {
            sender.send_replace(Some(Arc::new(
                FrameUpdate::new(
                    sequence,
                    1,
                    1,
                    remoteapp_rdp_core::PixelFormat::Rgba8888,
                    vec![0; 4],
                    vec![],
                )
                .unwrap(),
            )));
        }
        assert_eq!(c.frame().unwrap().sequence, 100);
        c.suspend(true);
        assert!(c.frame().is_none());
        c.suspend(false);
        assert_eq!(c.frame().unwrap().sequence, 100);
        c.disconnect();
        assert!(c.frame().is_none());
    }
}
