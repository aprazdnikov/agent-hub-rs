//! An OS setting with immediate application, separate from the bot's draft form.

use crate::autostart::{Action, Registration, State};
use eframe::egui;
use std::sync::{
    Arc,
    mpsc::{self, Receiver, TryRecvError},
};
use std::time::Duration;

struct Outcome {
    state: Result<State, String>,
    error: Option<String>,
}

pub struct Control {
    registration: Result<Arc<dyn Registration>, String>,
    state: Option<Result<State, String>>,
    error: Option<String>,
    pending: Option<Receiver<Outcome>>,
}

impl Control {
    pub fn new(registration: Result<Arc<dyn Registration>, String>) -> Self {
        Self { registration, state: None, error: None, pending: None }
    }

    fn request(&mut self, action: Action, ctx: &egui::Context) {
        if self.pending.is_some() {
            return;
        }
        let registration = match &self.registration {
            Ok(registration) => registration.clone(),
            Err(error) => {
                self.state = Some(Err(error.clone()));
                return;
            }
        };
        let (send, receive) = mpsc::channel();
        let context = ctx.clone();
        self.error = None;
        match std::thread::Builder::new().name("autostart".into()).spawn(move || {
            let outcome = match registration.apply(action) {
                Ok(state) => Outcome { state: Ok(state), error: None },
                Err(error) if action != Action::Inspect => {
                    Outcome { state: registration.apply(Action::Inspect), error: Some(error) }
                }
                Err(error) => Outcome { state: Err(error), error: None },
            };
            let _ = send.send(outcome);
            context.request_repaint();
        }) {
            Ok(_) => self.pending = Some(receive),
            Err(error) => {
                self.state =
                    Some(Err(format!("Не удалось запустить проверку автозапуска: {error}")));
            }
        }
    }

    pub fn poll(&mut self, ctx: &egui::Context) {
        if let Some(pending) = self.pending.take() {
            match pending.try_recv() {
                Ok(outcome) => {
                    self.state = Some(outcome.state);
                    self.error = outcome.error;
                }
                Err(TryRecvError::Empty) => {
                    self.pending = Some(pending);
                    ctx.request_repaint_after(Duration::from_millis(100));
                }
                Err(TryRecvError::Disconnected) => {
                    self.state = Some(Err("Проверка автозапуска прервана".into()));
                }
            }
        }
    }

    pub fn show(&mut self, ui: &mut egui::Ui) {
        self.poll(ui.ctx());
        if self.state.is_none() && self.pending.is_none() {
            self.request(Action::Inspect, ui.ctx());
        }
        ui.heading("Автозапуск приложения");
        match &self.state {
            Some(Ok(State::Enabled)) => {
                ui.label("Включён в настройках ОС");
            }
            Some(Ok(State::Disabled)) => {
                ui.label("Выключен в настройках ОС");
            }
            Some(Ok(State::NeedsAttention(reason))) => {
                ui.label(format!("Нужна проверка: {reason}"));
            }
            Some(Err(error)) => {
                ui.colored_label(ui.visuals().error_fg_color, error);
            }
            None => {
                ui.label("Состояние ещё не получено");
            }
        }
        if let Some(error) = &self.error {
            ui.colored_label(ui.visuals().error_fg_color, error);
        }
        let available = self.pending.is_none() && self.registration.is_ok();
        ui.horizontal(|ui| {
            if ui
                .add_enabled(
                    available && self.state != Some(Ok(State::Enabled)),
                    egui::Button::new("Включить"),
                )
                .clicked()
            {
                self.request(Action::Enable, ui.ctx());
            }
            if ui.add_enabled(available, egui::Button::new("Выключить")).clicked() {
                self.request(Action::Disable, ui.ctx());
            }
            if ui.add_enabled(available, egui::Button::new("Проверить состояние")).clicked()
            {
                self.request(Action::Inspect, ui.ctx());
            }
            if self.pending.is_some() {
                ui.spinner();
                ui.label("Обращение к ОС…");
            }
        });
        ui.small("Применяется сразу для текущего пользователя, отдельно от «Сохранить». При входе запускается только хаб; VPN и пользовательские действия автоматически не запускаются.");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    struct Fake {
        state: Mutex<State>,
        actions: Mutex<Vec<Action>>,
        fail: Mutex<bool>,
        gate: Mutex<Option<Receiver<()>>>,
    }
    impl Fake {
        fn new() -> Arc<Self> {
            Arc::new(Self {
                state: Mutex::new(State::Disabled),
                actions: Mutex::new(vec![]),
                fail: Mutex::new(false),
                gate: Mutex::new(None),
            })
        }
    }
    impl Registration for Fake {
        fn apply(&self, action: Action) -> Result<State, String> {
            self.actions.lock().unwrap().push(action);
            if let Some(gate) = self.gate.lock().unwrap().take() {
                gate.recv().unwrap();
            }
            if action != Action::Inspect && *self.fail.lock().unwrap() {
                return Err("permission denied".into());
            }
            let mut state = self.state.lock().unwrap();
            match action {
                Action::Enable => *state = State::Enabled,
                Action::Disable => *state = State::Disabled,
                Action::Inspect => {}
            }
            Ok(state.clone())
        }
    }

    fn settle(control: &mut Control, ctx: &egui::Context) {
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        while control.pending.is_some() {
            control.poll(ctx);
            assert!(std::time::Instant::now() < deadline);
            std::thread::yield_now();
        }
    }

    #[test]
    fn operation_failure_rechecks_os_and_refresh_sees_external_changes() {
        let ctx = egui::Context::default();
        let service = Fake::new();
        let mut control = Control::new(Ok(service.clone()));
        control.request(Action::Enable, &ctx);
        settle(&mut control, &ctx);
        assert_eq!(control.state, Some(Ok(State::Enabled)));
        *service.fail.lock().unwrap() = true;
        control.request(Action::Disable, &ctx);
        settle(&mut control, &ctx);
        assert_eq!(control.state, Some(Ok(State::Enabled)));
        assert_eq!(control.error.as_deref(), Some("permission denied"));
        *service.state.lock().unwrap() = State::Disabled;
        control.request(Action::Inspect, &ctx);
        settle(&mut control, &ctx);
        assert_eq!(control.state, Some(Ok(State::Disabled)));
        assert_eq!(control.error, None);
    }

    #[test]
    fn one_pending_operation_prevents_overlapping_mutations() {
        let ctx = egui::Context::default();
        let service = Fake::new();
        let (release, wait) = mpsc::channel();
        *service.gate.lock().unwrap() = Some(wait);
        let mut control = Control::new(Ok(service.clone()));
        control.request(Action::Enable, &ctx);
        control.request(Action::Disable, &ctx);
        release.send(()).unwrap();
        settle(&mut control, &ctx);
        assert_eq!(*service.actions.lock().unwrap(), vec![Action::Enable]);
        assert_eq!(control.state, Some(Ok(State::Enabled)));
    }
}
