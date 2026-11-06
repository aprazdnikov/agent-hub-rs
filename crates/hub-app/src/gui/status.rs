//! The «Статус» tab.

use eframe::egui;

use hub_core::domain::BackendKind;

use crate::gui::look::{active, agent_line, agent_title, status_text};
use crate::supervisor::{BotStatus, Command, DiagnosticStatus, OutboxStatus, Snapshot};

pub fn show(ui: &mut egui::Ui, snapshot: &Snapshot, unsaved: bool, send: &mut impl FnMut(Command)) {
    egui::ScrollArea::vertical().show(ui, |ui| {
        super::theme::page_heading(ui, "Статус", "Подключение к Telegram и готовность агентов.");
        super::theme::surface(ui).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.strong("Telegram-бот");
            ui.label(status_text(&snapshot.bot));
            if matches!(snapshot.bot, BotStatus::Retrying { .. }) {
                ui.ctx().request_repaint_after(std::time::Duration::from_secs(1));
            }
            if matches!(snapshot.bot, BotStatus::Running { .. }) {
                ui.weak("Запуск не подтверждает непрерывную связь с Telegram.");
                ui.label("Токен: принят Telegram при запуске.");
                match snapshot.chat_access {
                    Some(hub_telegram::telegram::ChatAccess::Visible) => {
                        ui.label("Группа: доступна при запуске.");
                    }
                    Some(hub_telegram::telegram::ChatAccess::Unseen) => {
                        ui.label("Группа: недоступна при запуске.");
                    }
                    None => {
                        ui.label("Группа: не проверена.");
                    }
                }
            }
            if snapshot.chat_access == Some(hub_telegram::telegram::ChatAccess::Unseen) {
                ui.colored_label(
                    ui.visuals().warn_fg_color,
                    "При запуске бот не увидел группу. Добавьте его в группу, проверьте её ID в настройках и перезапустите бота.",
                );
            }
            ui.horizontal(|ui| {
                let (start, stop) = match &snapshot.bot {
                    BotStatus::Unconfigured => (false, false),
                    BotStatus::Stopped | BotStatus::Failed(_) => (true, false),
                    BotStatus::Starting | BotStatus::Running { .. } | BotStatus::Retrying { .. } => (false, true),
                };
                if ui.add_enabled(start, egui::Button::new("▶ Запустить")).clicked() {
                    send(Command::Start);
                }
                if ui.add_enabled(stop, egui::Button::new("⏹ Остановить")).clicked() {
                    send(Command::Stop);
                }
            });
            ui.add_space(8.0);
            ui.horizontal_wrapped(|ui| {
                ui.label(format!("Активных сессий: {}", active(snapshot)));
                ui.separator();
                ui.label(format!("Тем: {}", snapshot.topics.len()));
            });
        });
        ui.add_space(18.0);
        show_diagnostics(ui, snapshot, unsaved, send);
        ui.add_space(18.0);
        super::theme::surface(ui).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.strong("Недоставленные ответы");
            match snapshot.outbox {
                OutboxStatus::Unknown => { ui.label("Количество: неизвестно (данные ещё не получены)."); }
                OutboxStatus::Unavailable => { ui.label("Количество: неизвестно (ошибка чтения хранилища)."); }
                OutboxStatus::Count(count) => { ui.label(format!("В очереди для текущей группы: {count}")); }
            }
            ui.weak("Бот автоматически повторяет доставку при временных сбоях. После остановки повторов запустите бота и используйте /outbox в нужной теме. При потере подтверждения возможны дубликаты.");
        });
        ui.add_space(18.0);
        super::theme::surface(ui).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.strong("Агенты");
            ui.weak("Проверка CLI не подтверждает вход и работу модели.");
            ui.add_space(8.0);
            egui::Grid::new("status_agents").num_columns(2).spacing([28.0, 16.0]).show(ui, |ui| {
                for kind in BackendKind::ALL {
                    let state = snapshot
                        .agents
                        .iter()
                        .find(|agent| agent.kind == kind)
                        .map(|agent| &agent.state);
                    ui.strong(agent_title(kind));
                    ui.add(egui::Label::new(agent_line(state)).wrap());
                    ui.end_row();
                }
            });
        });
        ui.add_space(12.0);
        ui.weak(format!("agent-hub {}", env!("CARGO_PKG_VERSION")));
    });
}

fn diagnostic_report(ui: &mut egui::Ui, report: &crate::diagnostics::Report) {
    use crate::diagnostics::{AgentCheck, WorkspaceCheck};
    use hub_telegram::telegram::{DiagnosticChatKind, DiagnosticStep};
    ui.label(match report.telegram.token {
        DiagnosticStep::Ok => "Токен: принят Telegram.",
        DiagnosticStep::InvalidToken => "Токен: отклонён. Проверьте токен в настройках.",
        DiagnosticStep::Unavailable | DiagnosticStep::Network => {
            "Токен: не проверен. Проверьте сеть и повторите проверку."
        }
        DiagnosticStep::TimedOut => {
            "Токен: время ожидания истекло. Проверьте сеть и повторите проверку."
        }
        DiagnosticStep::NotChecked => "Токен: не проверен.",
    });
    ui.label(match report.telegram.chat {
        DiagnosticStep::Ok => "Группа: доступна.",
        DiagnosticStep::Unavailable => {
            "Группа: недоступна. Добавьте бота в группу и проверьте её ID в настройках."
        }
        DiagnosticStep::Network | DiagnosticStep::TimedOut => {
            "Группа: не проверена. Проверьте сеть и повторите проверку."
        }
        DiagnosticStep::NotChecked | DiagnosticStep::InvalidToken => "Группа: не проверена.",
    });
    ui.label(match report.telegram.chat_kind {
        Some(DiagnosticChatKind::Forum) => "Тип группы: супергруппа с темами.",
        Some(DiagnosticChatKind::Supergroup) => {
            "Тип группы: супергруппа без тем. Включите темы в Telegram."
        }
        Some(DiagnosticChatKind::Group) => "Тип группы: обычная группа. Включите темы в Telegram.",
        Some(DiagnosticChatKind::Other) => "Тип чата: не группа. Укажите ID группы с темами.",
        None => "Тип группы: не проверен.",
    });
    ui.label(match report.telegram.manage_topics {
        Some(true) => "Право управлять темами: доступно по данным Telegram.",
        Some(false) => "Право управлять темами: отсутствует. Назначьте бота администратором с правом управления темами.",
        None => "Право управлять темами: не проверено.",
    });
    ui.label(match report.workspace {
        WorkspaceCheck::Ready => "Рабочий каталог: доступен.",
        WorkspaceCheck::Unavailable => "Рабочий каталог: недоступен. Проверьте путь в настройках.",
        WorkspaceCheck::TimedOut => {
            "Рабочий каталог: время ожидания истекло. Проверьте доступ к диску."
        }
    });
    let agent = match &report.agent {
        AgentCheck::Ready(version) => {
            format!("{} CLI: версия {version}.", report.agent_kind.name())
        }
        AgentCheck::Unavailable => format!(
            "{} CLI: недоступен или неподходящая версия. Проверьте путь и установку.",
            report.agent_kind.name()
        ),
        AgentCheck::TimedOut => format!(
            "{} CLI: время ожидания истекло. Проверьте установку.",
            report.agent_kind.name()
        ),
    };
    ui.label(agent);
    ui.weak("Проверка CLI не подтверждает вход и работу модели.");
}

fn show_diagnostics(
    ui: &mut egui::Ui,
    snapshot: &Snapshot,
    unsaved: bool,
    send: &mut impl FnMut(Command),
) {
    super::theme::surface(ui).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.strong("Диагностика сохранённых настроек");
        if unsaved {
            ui.colored_label(
                ui.visuals().warn_fg_color,
                "Есть несохранённые изменения: они не участвуют в проверке.",
            );
        }
        if snapshot.bot == BotStatus::Unconfigured {
            ui.label("Сохраните настройки, чтобы выполнить проверку.");
        }
        match &snapshot.diagnostics {
            DiagnosticStatus::Idle => {
                ui.label("Ещё не проверено.");
            }
            DiagnosticStatus::Running => {
                ui.spinner();
                ui.label("Проверка выполняется…");
            }
            DiagnosticStatus::Complete(report) => diagnostic_report(ui, report),
        }
        let enabled = !matches!(snapshot.bot, BotStatus::Unconfigured)
            && !matches!(snapshot.diagnostics, DiagnosticStatus::Running);
        if ui.add_enabled(enabled, egui::Button::new("Проверить настройки")).clicked()
        {
            send(Command::Diagnose);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diagnostics::{AgentCheck, Report, WorkspaceCheck};
    use hub_telegram::telegram::{DiagnosticChatKind, DiagnosticStep, TelegramDiagnostic};

    #[test]
    fn diagnostic_ui_marks_unsaved_changes_and_unknown_rights() {
        let snapshot = Snapshot {
            bot: BotStatus::Stopped,
            diagnostics: DiagnosticStatus::Complete(Report {
                workspace: WorkspaceCheck::Unavailable,
                agent: AgentCheck::Unavailable,
                agent_kind: BackendKind::Claude,
                telegram: TelegramDiagnostic {
                    token: DiagnosticStep::Ok,
                    chat: DiagnosticStep::Ok,
                    chat_kind: Some(DiagnosticChatKind::Forum),
                    manage_topics: None,
                },
            }),
            ..Snapshot::default()
        };
        let ctx = egui::Context::default();
        let mut output = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1000.0, 1000.0),
                )),
                ..Default::default()
            },
            |ui| {
                egui::CentralPanel::default().show(ui, |ui| {
                    show_diagnostics(ui, &snapshot, true, &mut |_| {});
                });
            },
        );
        output.textures_delta.clear();
        let labels: Vec<_> = output
            .shapes
            .iter()
            .filter_map(|shape| match &shape.shape {
                egui::Shape::Text(text) => Some(text.galley.text()),
                _ => None,
            })
            .collect();
        assert!(labels.contains(&"Есть несохранённые изменения: они не участвуют в проверке."));
        assert!(labels.contains(&"Право управлять темами: не проверено."));
        assert!(labels.contains(&"Рабочий каталог: недоступен. Проверьте путь в настройках."));
    }
}
