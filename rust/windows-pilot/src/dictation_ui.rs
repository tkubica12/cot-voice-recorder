use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use tokio::sync::{mpsc, oneshot};
use voiceprompt_client_protocol::delivery::{ClipboardError, DeliveryPolicy};
use voiceprompt_client_protocol::settings::AppSettings;
use voiceprompt_client_protocol::state::AuthState;
use windows::Win32::Foundation::HWND;
#[cfg(test)]
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::NativeError;
use crate::client::{ClientCommand, ClientStatus};
use crate::desktop::{DesktopGuard, Target};
use crate::dictation::{DeliveryResult, DictationCommand, DictationMode, DictationStage};
use crate::dictation_overlay::{Frame, Overlay};
use crate::hotkeys::{HotkeyEvent, Hotkeys};

#[cfg(test)]
#[path = "dictation_ui_visual_tests.rs"]
mod visual_tests;

pub(crate) struct DictationUi {
    overlay: Overlay,
    owner: HWND,
    shortcuts: Hotkeys,
    commands: mpsc::Sender<ClientCommand>,
    policy: Arc<DeliveryPolicy>,
    generation: u64,
    mode: DictationMode,
    busy: bool,
    guard: Option<DesktopGuard>,
    paste: Option<Paste>,
    last_stage: Option<(u64, DictationStage)>,
    dismiss_at: Option<Instant>,
    last_shortcuts: Option<(bool, String, String)>,
    enabled: bool,
    stop_requested: bool,
    cancel_requested: bool,
    shortcut_error: bool,
    staged_settings: Option<AuthState>,
}

struct Paste {
    text: String,
    cancelled: Arc<AtomicBool>,
    reply: oneshot::Sender<DeliveryResult>,
    next_attempt: Instant,
    attempts: u32,
    copied: bool,
    until: Instant,
}

impl DictationUi {
    pub fn replace_host(
        &mut self,
        commands: mpsc::Sender<ClientCommand>,
        policy: Arc<DeliveryPolicy>,
        enabled: bool,
    ) {
        self.commands = commands;
        self.policy = policy;
        self.enabled = enabled;
        self.staged_settings = None;
        self.busy = false;
        self.cancel_requested = false;
        self.guard = None;
        self.last_stage = None;
        self.dismiss_at = None;
        if let Some(paste) = self.paste.take() {
            paste.cancelled.store(true, Ordering::Release);
        }
        self.hide();
    }

    pub fn new(
        owner: HWND,
        commands: mpsc::Sender<ClientCommand>,
        policy: Arc<DeliveryPolicy>,
        enabled: bool,
    ) -> Result<Self, NativeError> {
        let overlay = Overlay::new()?;
        Ok(Self {
            overlay,
            owner,
            shortcuts: Hotkeys::new(owner),
            commands,
            policy,
            generation: 0,
            mode: DictationMode::Hold,
            busy: false,
            guard: None,
            paste: None,
            last_stage: None,
            dismiss_at: None,
            last_shortcuts: None,
            enabled,
            stop_requested: false,
            cancel_requested: false,
            shortcut_error: false,
            staged_settings: None,
        })
    }

    pub fn configure(
        &mut self,
        settings: &AppSettings,
        auth: AuthState,
    ) -> Result<(), NativeError> {
        if self.staged_settings == Some(auth) {
            return Ok(());
        }
        self.staged_settings = None;
        let enabled = self.enabled && settings.dictation_enabled && auth == AuthState::SignedIn;
        let config = (
            enabled,
            settings.dictation_shortcut.clone(),
            settings.dictation_toggle_shortcut.clone(),
        );
        if self.last_shortcuts.as_ref() == Some(&config) {
            return Ok(());
        }

        let result = self.shortcuts.configure(enabled, &config.1, &config.2);
        self.shortcut_error = result.is_err();
        self.last_shortcuts = Some(config);
        result
    }

    pub fn validate_settings(
        &mut self,
        settings: &AppSettings,
        auth: AuthState,
        retain: bool,
    ) -> Result<(), NativeError> {
        self.staged_settings = None;
        if self.shortcut_error {
            self.last_shortcuts = None;
        }
        self.configure(settings, auth)?;
        if retain {
            self.staged_settings = Some(auth);
        }
        Ok(())
    }

    fn send(&self, command: DictationCommand) -> Result<(), NativeError> {
        self.commands
            .try_send(ClientCommand::Dictation(command))
            .map_err(|_| NativeError::Verification("Dictation command queue is busy or stopped."))
    }

    fn begin(&mut self, mode: DictationMode) -> Result<(), NativeError> {
        if self.staged_settings.is_some() {
            return Err(NativeError::Verification(
                "Wait for Settings to be applied before starting dictation.",
            ));
        }
        if self.busy || !self.enabled {
            return Ok(());
        }
        self.shortcuts.enable_cancel(true)?;
        self.generation = self.generation.wrapping_add(1);
        self.mode = mode;
        self.busy = true;
        self.stop_requested = false;
        self.cancel_requested = false;
        self.dismiss_at = None;
        self.hide();
        self.guard = if mode == DictationMode::Hold {
            Some(DesktopGuard::new(Target::capture()))
        } else {
            None
        };
        self.policy.set_dictation_busy(true);
        if let Err(error) = self.send(DictationCommand::Begin {
            generation: self.generation,
            mode,
        }) {
            self.busy = false;
            self.shortcuts.enable_cancel(false)?;
            self.policy.set_dictation_busy(false);
            return Err(error);
        }
        Ok(())
    }

    fn event(&mut self, event: HotkeyEvent) -> Result<(), NativeError> {
        match event {
            HotkeyEvent::HoldStart if !self.busy => self.begin(DictationMode::Hold),
            HotkeyEvent::HoldStop
                if self.busy && !self.stop_requested && self.mode == DictationMode::Hold =>
            {
                self.stop_requested = true;
                self.send(DictationCommand::Stop {
                    generation: self.generation,
                })
            }
            HotkeyEvent::Toggle if !self.busy => self.begin(DictationMode::Toggle),
            HotkeyEvent::Toggle if !self.stop_requested && self.mode == DictationMode::Toggle => {
                self.stop_requested = true;
                self.guard = Some(DesktopGuard::new(Target::capture()));
                self.send(DictationCommand::Stop {
                    generation: self.generation,
                })
            }
            HotkeyEvent::Cancel if self.busy && !self.cancel_requested => {
                self.cancel_requested = true;
                self.stop_requested = true;
                self.guard = None;
                self.hide();
                if let Some(paste) = self.paste.take() {
                    paste.cancelled.store(true, Ordering::Release);
                    let _ = paste.reply.send(DeliveryResult::CopiedOnly);
                }
                self.send(DictationCommand::Discard)
            }
            _ => Ok(()),
        }
    }

    pub fn hotkey(&mut self, id: i32, value: usize) -> Result<(), NativeError> {
        if let Some(event) = self.shortcuts.message(id, value) {
            self.event(event)?;
        }
        Ok(())
    }

    pub fn recover(&mut self, id: String) -> Result<(), NativeError> {
        if !self.enabled {
            return Err(NativeError::Verification(
                "Recovery actions are disabled in this read-only client.",
            ));
        }
        if self.busy {
            return Err(NativeError::Verification(
                "Finish dictation before Recovery.",
            ));
        }
        self.generation = self.generation.wrapping_add(1);
        self.mode = DictationMode::Toggle;
        self.guard = None;
        self.busy = true;
        self.cancel_requested = false;
        self.stop_requested = true;
        self.shortcuts.enable_cancel(true)?;
        if let Err(error) = self.send(DictationCommand::Recover {
            id,
            generation: self.generation,
        }) {
            self.busy = false;
            self.shortcuts.enable_cancel(false)?;
            return Err(error);
        }
        self.show(
            "Recovering saved audio - Esc cancels",
            "No automatic paste.",
            false,
        )
    }
    pub fn poll(&mut self, status: &ClientStatus) -> Result<(), NativeError> {
        self.poll_at(status, Instant::now())
    }

    fn poll_at(&mut self, status: &ClientStatus, now: Instant) -> Result<(), NativeError> {
        if self.shortcuts.needs_poll() {
            if let Some(event) = self.shortcuts.poll() {
                self.event(event)?;
            }
        }
        if self.busy {
            if let Some(guard) = self.guard.as_mut() {
                guard.observe();
            }
        }
        if self.generation == status.dictation.generation {
            let view = &status.dictation;
            let changed = self.last_stage != Some((view.generation, view.stage));
            if changed {
                self.dismiss_at = None;
            }
            if self.cancel_requested {
                if matches!(
                    view.stage,
                    DictationStage::Idle
                        | DictationStage::Pasted
                        | DictationStage::Saved
                        | DictationStage::PasteSkipped
                        | DictationStage::Failed
                ) {
                    self.busy = false;
                    self.shortcuts.enable_cancel(false)?;
                }
                self.hide();
            } else {
                match view.stage {
                    DictationStage::Starting => self.hide(),
                    DictationStage::Recording => {
                        let elapsed = view
                            .recording_started
                            .map_or(Duration::ZERO, |start| now.saturating_duration_since(start))
                            .as_secs();
                        let instruction = if view.mode == DictationMode::Hold {
                            "release to finish"
                        } else {
                            "press toggle to finish"
                        };
                        self.show(
                            &format!(
                                "Listening {:02}:{:02} - {instruction}",
                                elapsed / 60,
                                elapsed % 60
                            ),
                            &view.preview,
                            true,
                        )?;
                    }
                    DictationStage::Finishing => self.show(
                        "Finishing transcription - Esc discards",
                        &view.preview,
                        false,
                    )?,
                    DictationStage::Idle | DictationStage::Pasted => {
                        self.busy = false;
                        self.shortcuts.enable_cancel(false)?;
                        self.hide();
                    }
                    DictationStage::Saved | DictationStage::Failed => {
                        self.busy = false;
                        self.shortcuts.enable_cancel(false)?;
                        self.hide();
                    }
                    DictationStage::PasteSkipped => {
                        self.busy = false;
                        self.shortcuts.enable_cancel(false)?;
                        if changed {
                            self.show("Saved - paste skipped", &view.preview, false)?;
                            self.dismiss_at = Some(now + Duration::from_millis(900));
                        }
                    }
                }
                if changed
                    && matches!(
                        view.stage,
                        DictationStage::Idle
                            | DictationStage::Saved
                            | DictationStage::Failed
                            | DictationStage::PasteSkipped
                            | DictationStage::Pasted
                    )
                {
                    if let Some(issue) = view.issue {
                        crate::platform::notify(self.owner, "VoicePrompt", issue, true)?;
                    }
                }
            }
            self.last_stage = Some((view.generation, view.stage));
        }
        if self.dismiss_at.is_some_and(|deadline| now >= deadline) {
            self.hide();
            self.dismiss_at = None;
        }
        self.process_paste()?;
        Ok(())
    }

    pub fn deliver(
        &mut self,
        generation: u64,
        text: String,
        cancelled: Arc<AtomicBool>,
        reply: oneshot::Sender<DeliveryResult>,
    ) {
        if generation != self.generation
            || !self.busy
            || self.cancel_requested
            || self.paste.is_some()
        {
            let _ = reply.send(DeliveryResult::CopiedOnly);
            return;
        }
        self.paste = Some(Paste {
            text,
            cancelled,
            reply,
            attempts: 0,
            copied: false,
            next_attempt: Instant::now(),
            until: Instant::now() + Duration::from_secs(2),
        });
    }

    fn process_paste(&mut self) -> Result<(), NativeError> {
        let Some(mut paste) = self.paste.take() else {
            return Ok(());
        };
        if !self.can_process_paste(&paste) {
            let _ = paste.reply.send(DeliveryResult::CopiedOnly);
            return Ok(());
        }
        if Instant::now() >= paste.until {
            let _ = paste.reply.send(if paste.copied {
                DeliveryResult::CopiedOnly
            } else {
                DeliveryResult::ClipboardFailed
            });
            return Ok(());
        }
        if !paste.copied {
            if Instant::now() < paste.next_attempt {
                self.paste = Some(paste);
                return Ok(());
            }
            paste.attempts += 1;
            match crate::clipboard::write_with_sequence(self.overlay.window, &paste.text) {
                Ok(sequence) => {
                    paste.copied = true;
                    paste.until = Instant::now() + Duration::from_millis(750);
                    if let Some(guard) = self.guard.as_mut() {
                        guard.mark_clipboard(sequence);
                    }
                }
                Err(ClipboardError::Busy) if paste.attempts < 5 => {
                    paste.next_attempt =
                        Instant::now() + Duration::from_millis(50 * u64::from(paste.attempts));
                    self.paste = Some(paste);
                    return Ok(());
                }
                Err(error) => {
                    eprintln!("Dictation clipboard failed: {error}");
                    let _ = paste.reply.send(DeliveryResult::ClipboardFailed);
                    return Ok(());
                }
            }
        }
        let result = match self.guard.as_mut() {
            Some(guard) => {
                if !guard.observe() {
                    let _ = paste.reply.send(DeliveryResult::CopiedOnly);
                    return Ok(());
                }
                if !DesktopGuard::modifiers_released() {
                    self.paste = Some(paste);
                    return Ok(());
                }
                if guard.try_paste() {
                    DeliveryResult::Pasted
                } else {
                    DeliveryResult::CopiedOnly
                }
            }
            _ => DeliveryResult::CopiedOnly,
        };
        if result == DeliveryResult::Pasted {
            self.hide();
        }
        let _ = paste.reply.send(result);
        Ok(())
    }

    fn can_process_paste(&self, paste: &Paste) -> bool {
        self.enabled
            && self.busy
            && !self.cancel_requested
            && !paste.cancelled.load(Ordering::Acquire)
            && !paste.reply.is_closed()
            && self.policy.authenticated()
    }

    fn show(&mut self, header: &str, preview: &str, recording: bool) -> Result<(), NativeError> {
        self.overlay.present(Frame::new(header, preview, recording))
    }

    pub fn hide(&mut self) {
        self.overlay.hide();
    }

    pub fn shortcut_error(&self) -> bool {
        self.shortcut_error
    }

    pub fn fast_poll(&self) -> bool {
        self.busy
            || self.paste.is_some()
            || self.dismiss_at.is_some()
            || self.shortcuts.needs_poll()
    }
}

impl Drop for DictationUi {
    fn drop(&mut self) {
        if let Some(paste) = self.paste.take() {
            paste.cancelled.store(true, Ordering::Release);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn staged_settings_remain_live_until_host_reload_without_enabling_fixture_devices() {
        let previous_focus = unsafe { GetForegroundWindow() };
        let (commands, _receiver) = mpsc::channel(1);
        let mut ui = DictationUi::new(
            HWND::default(),
            commands,
            Arc::new(DeliveryPolicy::default()),
            false,
        )
        .unwrap();
        let next = AppSettings {
            dictation_shortcut: "Ctrl+Alt+F9".into(),
            dictation_toggle_shortcut: "Ctrl+Alt+Shift+F9".into(),
            ..Default::default()
        };
        ui.validate_settings(&next, AuthState::SignedIn, true)
            .unwrap();
        let staged = ui.last_shortcuts.clone();
        ui.configure(&AppSettings::default(), AuthState::SignedIn)
            .unwrap();
        assert_eq!(ui.last_shortcuts, staged);
        assert!(ui.begin(DictationMode::Hold).is_err());
        ui.validate_settings(&AppSettings::default(), AuthState::SignedIn, false)
            .unwrap();
        assert!(ui.staged_settings.is_none());
        assert!(!unsafe { IsWindowVisible(ui.overlay.window) }.as_bool());
        assert_eq!(unsafe { GetForegroundWindow() }, previous_focus);
    }

    fn pending_ui() -> (DictationUi, mpsc::Receiver<ClientCommand>) {
        let (commands, receiver) = mpsc::channel(2);
        let mut ui = DictationUi::new(
            HWND::default(),
            commands,
            Arc::new(DeliveryPolicy::default()),
            false,
        )
        .unwrap();
        ui.generation = 17;
        ui.busy = true;
        (ui, receiver)
    }

    #[test]
    fn read_only_recovery_refuses_before_shortcuts_generation_or_command_queue() {
        let (mut ui, mut commands) = pending_ui();
        ui.busy = false;
        let generation = ui.generation;
        assert!(
            ui.recover("synthetic never open any journal".into())
                .is_err()
        );
        assert_eq!(ui.generation, generation);
        assert!(!ui.busy);
        assert!(!ui.shortcuts.needs_poll());
        assert!(commands.try_recv().is_err());
        assert!(!unsafe { IsWindowVisible(ui.overlay.window) }.as_bool());
    }

    #[test]
    fn live_clock_preview_and_single_terminal_dismiss_match_reference_without_physical_ports() {
        let (mut ui, _commands) = pending_ui();
        ui.overlay.suppress_presentation = true;
        let mut status = ClientStatus::default();
        let started = Instant::now();
        status.dictation.generation = 17;
        status.dictation.recording_started = Some(started);
        status.dictation.stage = DictationStage::Recording;
        ui.poll_at(&status, started).unwrap();
        assert_eq!(
            ui.overlay.fixture_frame().unwrap().header,
            "Listening 00:00 - release to finish"
        );
        assert_eq!(
            ui.overlay.fixture_frame().unwrap().preview,
            crate::dictation_overlay::PLACEHOLDER
        );
        status.dictation.mode = DictationMode::Toggle;
        status.dictation.preview = "Synthetic growing preview".into();
        ui.poll_at(&status, started + Duration::from_secs(61))
            .unwrap();
        assert_eq!(
            ui.overlay.fixture_frame().unwrap().header,
            "Listening 01:01 - press toggle to finish"
        );
        assert_eq!(
            ui.overlay.fixture_frame().unwrap().preview,
            "Synthetic growing preview"
        );
        assert!(ui.overlay.fixture_frame().unwrap().recording);
        status.dictation.stage = DictationStage::Finishing;
        ui.poll_at(&status, started + Duration::from_secs(62))
            .unwrap();
        assert_eq!(
            ui.overlay.fixture_frame().unwrap().header,
            "Finishing transcription - Esc discards"
        );
        assert!(!ui.overlay.fixture_frame().unwrap().recording);
        status.dictation.stage = DictationStage::PasteSkipped;
        status.dictation.preview = "Only confirmed final text.".into();
        let terminal = started + Duration::from_secs(63);
        ui.poll_at(&status, terminal).unwrap();
        assert_eq!(
            ui.overlay.fixture_frame().unwrap().header,
            "Saved - paste skipped"
        );
        assert_eq!(
            ui.overlay.fixture_frame().unwrap().preview,
            "Only confirmed final text."
        );
        ui.poll_at(&status, terminal + Duration::from_millis(899))
            .unwrap();
        assert!(ui.overlay.fixture_frame().is_some());
        ui.poll_at(&status, terminal + Duration::from_millis(900))
            .unwrap();
        assert!(ui.overlay.fixture_frame().is_none());
        ui.poll_at(&status, terminal + Duration::from_secs(5))
            .unwrap();
        assert!(
            ui.overlay.fixture_frame().is_none(),
            "Unchanged terminal state must not rearm the dismiss timer."
        );
        assert!(ui.paste.is_none());
        assert!(!ui.enabled);
    }

    #[test]
    fn escape_rejects_late_delivery_before_the_background_actor_observes_discard() {
        let (mut ui, mut commands) = pending_ui();
        ui.event(HotkeyEvent::Cancel).unwrap();
        assert!(matches!(
            commands.try_recv().unwrap(),
            ClientCommand::Dictation(DictationCommand::Discard)
        ));
        let (reply, mut result) = oneshot::channel();
        ui.deliver(
            17,
            "synthetic must not reach clipboard".into(),
            Arc::new(AtomicBool::new(false)),
            reply,
        );
        assert!(ui.paste.is_none());
        assert_eq!(result.try_recv().unwrap(), DeliveryResult::CopiedOnly);
        assert!(
            ui.busy,
            "A new capture must wait for the old actor to finish."
        );
        ui.event(HotkeyEvent::Cancel).unwrap();
        assert!(
            commands.try_recv().is_err(),
            "Repeated Escape must not flood the actor."
        );
    }

    #[test]
    fn escape_cancels_already_queued_paste_and_late_finishing_status_cannot_redisplay_it() {
        let (mut ui, _commands) = pending_ui();
        let cancelled = Arc::new(AtomicBool::new(false));
        let (reply, mut result) = oneshot::channel();
        ui.deliver(17, "synthetic".into(), cancelled.clone(), reply);
        assert!(ui.paste.is_some());
        ui.event(HotkeyEvent::Cancel).unwrap();
        assert!(cancelled.load(Ordering::Acquire));
        assert_eq!(result.try_recv().unwrap(), DeliveryResult::CopiedOnly);
        let mut status = ClientStatus::default();
        status.dictation.generation = 17;
        status.dictation.stage = DictationStage::Finishing;
        ui.poll(&status).unwrap();
        assert!(!unsafe { IsWindowVisible(ui.overlay.window) }.as_bool());
        assert!(ui.busy);
        status.dictation.stage = DictationStage::Idle;
        ui.poll(&status).unwrap();
        assert!(!ui.busy);
        assert!(ui.paste.is_none());
    }

    #[test]
    fn disabled_physical_ports_reject_even_an_accidentally_queued_authenticated_delivery() {
        let (mut ui, _commands) = pending_ui();
        let (reply, result) = oneshot::channel();
        ui.deliver(
            17,
            "synthetic must never touch system clipboard".into(),
            Arc::new(AtomicBool::new(false)),
            reply,
        );
        assert!(ui.paste.is_some());
        assert!(!ui.can_process_paste(ui.paste.as_ref().unwrap()));
        assert!(!unsafe { IsWindowVisible(ui.overlay.window) }.as_bool());
        drop(ui);
        assert!(result.blocking_recv().is_err());
    }
}
