use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use tokio::sync::{mpsc, oneshot};
use voiceprompt_client_protocol::delivery::{ClipboardError, DeliveryPolicy};
use voiceprompt_client_protocol::settings::AppSettings;
use voiceprompt_client_protocol::state::AuthState;
use windows::Win32::Foundation::HWND;
use windows::Win32::Graphics::Gdi::{DEFAULT_GUI_FONT, GetStockObject};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{PCWSTR, w};

use crate::NativeError;
use crate::client::{ClientCommand, ClientStatus};
use crate::desktop::{DesktopGuard, Target};
use crate::dictation::{DeliveryResult, DictationCommand, DictationMode, DictationStage};
use crate::hotkeys::{HotkeyEvent, Hotkeys};

pub(crate) struct DictationUi {
    overlay: HWND,
    shortcuts: Hotkeys,
    commands: mpsc::Sender<ClientCommand>,
    policy: Arc<DeliveryPolicy>,
    generation: u64,
    mode: DictationMode,
    busy: bool,
    guard: Option<DesktopGuard>,
    paste: Option<Paste>,
    last_overlay: String,
    last_shortcuts: Option<(bool, String, String)>,
    enabled: bool,
    stop_requested: bool,
    cancel_requested: bool,
    shortcut_error: bool,
    staged_settings: Option<AuthState>,
    font: crate::dpi::DpiFont,
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
        let font = crate::dpi::DpiFont::create(crate::dpi::window_dpi(owner))?;
        let overlay = unsafe {
            CreateWindowExW(
                WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW | WS_EX_TOPMOST,
                w!("STATIC"),
                PCWSTR::null(),
                WS_POPUP | WS_BORDER,
                0,
                0,
                440,
                120,
                None,
                None,
                None,
                None,
            )
        }
        .map_err(|error| NativeError::Api("Create nonactivating dictation overlay", error))?;
        unsafe {
            let _ = SendMessageW(
                overlay,
                WM_SETFONT,
                Some(windows::Win32::Foundation::WPARAM(
                    GetStockObject(DEFAULT_GUI_FONT).0 as usize,
                )),
                None,
            );
        }
        font.apply(overlay);
        Ok(Self {
            overlay,
            shortcuts: Hotkeys::new(owner),
            commands,
            policy,
            generation: 0,
            mode: DictationMode::Hold,
            busy: false,
            guard: None,
            paste: None,
            last_overlay: String::new(),
            last_shortcuts: None,
            enabled,
            stop_requested: false,
            cancel_requested: false,
            shortcut_error: false,
            staged_settings: None,
            font,
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
        self.show("Starting microphone...\r\n\r\nEsc discards")?;
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
        self.show("Recovering saved audio...\r\nNo automatic paste. Esc cancels recovery.")
    }
    pub fn poll(&mut self, status: &ClientStatus) -> Result<(), NativeError> {
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
            if self.cancel_requested {
                if matches!(
                    view.stage,
                    DictationStage::Idle
                        | DictationStage::Pasted
                        | DictationStage::Saved
                        | DictationStage::Failed
                ) {
                    self.busy = false;
                    self.shortcuts.enable_cancel(false)?;
                }
                self.hide();
            } else {
                match view.stage {
                    DictationStage::Recording => {
                        let title = if view.mode == DictationMode::Hold {
                            "Listening - release to finish"
                        } else {
                            "Listening - press toggle to finish"
                        };
                        self.show(&format!("{title}\r\n{}\r\nEsc discards", view.preview))?;
                    }
                    DictationStage::Finishing => self.show(&format!(
                        "Finishing transcription - Esc discards\r\n{}",
                        view.preview
                    ))?,
                    DictationStage::Idle | DictationStage::Pasted => {
                        self.busy = false;
                        self.shortcuts.enable_cancel(false)?;
                        self.hide();
                    }
                    DictationStage::Saved | DictationStage::Failed => {
                        self.busy = false;
                        self.shortcuts.enable_cancel(false)?;
                        self.show(if view.stage == DictationStage::Failed {
                        "Dictation failed - open Settings or Recovery.\r\nNothing was pasted."
                    } else { "Saved - paste skipped or transcription pending.\r\nUse History or Recovery." })?;
                    }
                    _ => {}
                }
            }
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
        if paste.cancelled.load(Ordering::Acquire)
            || self.cancel_requested
            || paste.reply.is_closed()
            || !self.busy
            || !self.policy.authenticated()
        {
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
            match crate::clipboard::write_with_sequence(self.overlay, &paste.text) {
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

    fn show(&mut self, text: &str) -> Result<(), NativeError> {
        if self.last_overlay != text {
            let encoded: Vec<_> = text.encode_utf16().chain(std::iter::once(0)).collect();
            unsafe { SetWindowTextW(self.overlay, PCWSTR(encoded.as_ptr())) }
                .map_err(|error| NativeError::Api("Update dictation overlay", error))?;
            self.last_overlay = text.into();
        }
        let dpi = crate::dpi::window_dpi(self.overlay);
        if self.font.dpi != dpi {
            self.font = crate::dpi::DpiFont::create(dpi)?;
            self.font.apply(self.overlay);
        }
        let scale = |value: i32| value * dpi as i32 / 96;
        let x = (unsafe { GetSystemMetrics(SM_CXSCREEN) } - scale(460)).max(0);
        let y = (unsafe { GetSystemMetrics(SM_CYSCREEN) } - scale(200)).max(0);
        unsafe {
            SetWindowPos(
                self.overlay,
                Some(HWND_TOPMOST),
                x,
                y,
                scale(440),
                scale(120),
                SWP_NOACTIVATE | SWP_SHOWWINDOW,
            )
        }
        .map_err(|error| NativeError::Api("Show nonactivating dictation overlay", error))
    }

    pub fn hide(&self) {
        unsafe {
            let _ = ShowWindow(self.overlay, SW_HIDE);
        }
    }

    pub fn shortcut_error(&self) -> bool {
        self.shortcut_error
    }

    pub fn fast_poll(&self) -> bool {
        self.busy || self.paste.is_some() || self.shortcuts.needs_poll()
    }
}

impl Drop for DictationUi {
    fn drop(&mut self) {
        if let Some(paste) = self.paste.take() {
            paste.cancelled.store(true, Ordering::Release);
        }
        if let Err(error) = unsafe { DestroyWindow(self.overlay) } {
            eprintln!("Overlay cleanup failed: {error}");
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
        assert!(!unsafe { IsWindowVisible(ui.overlay) }.as_bool());
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
        assert!(!unsafe { IsWindowVisible(ui.overlay) }.as_bool());
        assert!(ui.busy);
        status.dictation.stage = DictationStage::Idle;
        ui.poll(&status).unwrap();
        assert!(!ui.busy);
        assert!(ui.paste.is_none());
    }
}
