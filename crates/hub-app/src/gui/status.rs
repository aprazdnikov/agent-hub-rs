//! Connection status and the saved-settings diagnostic checks.

use eframe::egui;
use kylig_ui::{Button, ProgressRing};

use crate::diagnostics::{AgentCheck, Report, WorkspaceCheck};
use crate::gui::theme;
use hub_core::domain::BackendKind;
use hub_telegram::telegram::{DiagnosticChatKind, DiagnosticStep, TelegramDiagnostic};

use crate::gui::look::{active, agent_line, agent_title, status_text};
use crate::supervisor::{AgentState, BotStatus, Command, DiagnosticStatus, OutboxStatus, Snapshot};

pub fn show(ui: &mut egui::Ui, snapshot: &Snapshot, unsaved: bool, send: &mut impl FnMut(Command)) {
    ui.add_space(8.0);
    theme::card(ui).show(ui, |ui| {
        ui.set_min_width(ui.available_width());
        bot_controls(ui, snapshot, send);
    });
    show_diagnostics(ui, snapshot, unsaved, send);
    theme::card(ui).show(ui, |ui| {
        ui.set_min_width(ui.available_width());
        ui.strong("Агенты");
        for kind in BackendKind::ALL {
            let state = snapshot.agents.iter().find(|agent| agent.kind == kind).map(|agent| &agent.state);
            ui.horizontal_wrapped(|ui| {
                ui.label(agent_title(kind));
                match state {
                    Some(AgentState::Unavailable(_)) => ui.colored_label(ui.visuals().warn_fg_color, agent_line(state)),
                    Some(AgentState::Ready { .. }) => ui.weak(agent_line(state)),
                    None => ui.weak("CLI ещё не проверен"),
                };
            });
        }
        ui.weak("Проверка CLI не подтверждает вход и работу модели.");
        ui.separator();
        match snapshot.outbox {
            OutboxStatus::Unknown => ui.weak("Недоставленные ответы • данные ещё не получены"),
            OutboxStatus::Unavailable => ui.colored_label(ui.visuals().warn_fg_color, "Недоставленные ответы • ошибка чтения хранилища"),
            OutboxStatus::Count(0) => ui.label("Недоставленные ответы • очередь пуста"),
            OutboxStatus::Count(count) => ui.label(format!("Недоставленные ответы • в очереди для текущей группы: {count}")),
        };
        ui.weak("Бот автоматически повторяет доставку при временных сбоях. После остановки повторов запустите бота и используйте /outbox в нужной теме. При потере подтверждения возможны дубликаты.");
    });
    ui.weak(format!("agent-hub {}", env!("CARGO_PKG_VERSION")));
}

fn bot_controls(ui: &mut egui::Ui, snapshot: &Snapshot, send: &mut impl FnMut(Command)) {
    ui.strong("Telegram-бот");
    ui.horizontal_wrapped(|ui| {
        if matches!(snapshot.bot, BotStatus::Starting | BotStatus::Retrying { .. }) {
            ui.add(ProgressRing::indeterminate().size(18.0));
        }
        if snapshot.local_only && snapshot.bot == BotStatus::Unconfigured {
            ui.weak("Локальный режим. Подключение Telegram выключено.");
        } else {
            ui.label(status_text(&snapshot.bot));
        }
    });
    if matches!(snapshot.bot, BotStatus::Retrying { .. }) {
        ui.ctx().request_repaint_after(std::time::Duration::from_secs(1));
    }
    if matches!(snapshot.bot, BotStatus::Running { .. }) {
        ui.weak("Запуск не подтверждает непрерывную связь с Telegram.");
        ui.label("Токен: принят Telegram при запуске.");
        ui.label(match snapshot.chat_access {
            Some(hub_telegram::telegram::ChatAccess::Visible) => "Группа: доступна при запуске.",
            Some(hub_telegram::telegram::ChatAccess::Unseen) => "Группа: недоступна при запуске.",
            None => "Группа: не проверена.",
        });
    }
    if snapshot.chat_access == Some(hub_telegram::telegram::ChatAccess::Unseen) {
        ui.colored_label(ui.visuals().warn_fg_color, "При запуске бот не увидел группу. Добавьте его в группу, проверьте её ID в настройках и перезапустите бота.");
    }
    ui.weak(format!("Активных сессий: {}  •  Тем: {}", active(snapshot), snapshot.topics.len()));
    match &snapshot.bot {
        BotStatus::Unconfigured => {}
        BotStatus::Stopped | BotStatus::Failed(_) => {
            if ui.add(Button::new("Запустить бота").icon(kylig_icons::PLAY)).clicked()
            {
                send(Command::Start);
            }
        }
        BotStatus::Starting | BotStatus::Running { .. } | BotStatus::Retrying { .. } => {
            if ui.add(Button::new("Остановить бота").icon(kylig_icons::STOP)).clicked()
            {
                send(Command::Stop);
            }
        }
    }
}

fn telegram_report(ui: &mut egui::Ui, telegram: &TelegramDiagnostic) -> bool {
    let token = match telegram.token {
        DiagnosticStep::Ok => "Токен: принят Telegram.",
        DiagnosticStep::InvalidToken => "Токен: отклонён. Проверьте токен в настройках.",
        DiagnosticStep::Unavailable | DiagnosticStep::Network => {
            "Токен: не проверен. Проверьте сеть и повторите проверку."
        }
        DiagnosticStep::TimedOut => {
            "Токен: время ожидания истекло. Проверьте сеть и повторите проверку."
        }
        DiagnosticStep::NotChecked => "Токен: не проверен.",
    };
    let failure = |step| !matches!(step, DiagnosticStep::Ok | DiagnosticStep::NotChecked);
    diagnostic_line(ui, token, failure(telegram.token));
    let chat = match telegram.chat {
        DiagnosticStep::Ok => "Группа: доступна.",
        DiagnosticStep::Unavailable => {
            "Группа: недоступна. Добавьте бота в группу и проверьте её ID в настройках."
        }
        DiagnosticStep::Network | DiagnosticStep::TimedOut => {
            "Группа: не проверена. Проверьте сеть и повторите проверку."
        }
        DiagnosticStep::NotChecked | DiagnosticStep::InvalidToken => "Группа: не проверена.",
    };
    diagnostic_line(ui, chat, failure(telegram.chat));
    let kind = match telegram.chat_kind {
        Some(DiagnosticChatKind::Forum) => "Тип группы: супергруппа с темами.",
        Some(DiagnosticChatKind::Supergroup) => {
            "Тип группы: супергруппа без тем. Включите темы в Telegram."
        }
        Some(DiagnosticChatKind::Group) => "Тип группы: обычная группа. Включите темы в Telegram.",
        Some(DiagnosticChatKind::Other) => "Тип чата: не группа. Укажите ID группы с темами.",
        None => "Тип группы: не проверен.",
    };
    diagnostic_line(
        ui,
        kind,
        telegram.chat_kind.is_some_and(|kind| kind != DiagnosticChatKind::Forum),
    );
    let rights = match telegram.manage_topics {
        Some(true) => "Право управлять темами: доступно по данным Telegram.",
        Some(false) => {
            "Право управлять темами: отсутствует. Назначьте бота администратором с правом управления темами."
        }
        None => "Право управлять темами: не проверено.",
    };
    diagnostic_line(ui, rights, telegram.manage_topics == Some(false));
    telegram.token != DiagnosticStep::Ok
        || telegram.chat != DiagnosticStep::Ok
        || telegram.chat_kind != Some(DiagnosticChatKind::Forum)
        || telegram.manage_topics != Some(true)
}

fn diagnostic_line(ui: &mut egui::Ui, text: &str, warn: bool) {
    if warn {
        ui.colored_label(ui.visuals().warn_fg_color, text);
    } else {
        ui.label(text);
    }
}

fn diagnostic_report(ui: &mut egui::Ui, report: &Report, local: bool) -> bool {
    let retry_telegram = if local {
        ui.weak("Telegram пропущен в локальном режиме.");
        false
    } else {
        telegram_report(ui, &report.telegram)
    };
    let workspace = match report.workspace {
        WorkspaceCheck::Ready => "Рабочий каталог: доступен.",
        WorkspaceCheck::Unavailable => "Рабочий каталог: недоступен. Проверьте путь в настройках.",
        WorkspaceCheck::TimedOut => {
            "Рабочий каталог: время ожидания истекло. Проверьте доступ к диску."
        }
    };
    let retry_workspace = report.workspace != WorkspaceCheck::Ready;
    diagnostic_line(ui, workspace, retry_workspace);
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
    let retry_agent = !matches!(report.agent, AgentCheck::Ready(_));
    diagnostic_line(ui, &agent, retry_agent);
    retry_telegram || retry_workspace || retry_agent
}

fn show_diagnostics(
    ui: &mut egui::Ui,
    snapshot: &Snapshot,
    unsaved: bool,
    send: &mut impl FnMut(Command),
) {
    theme::card(ui).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.strong("Проверка сохранённых настроек");
        if unsaved {
            ui.colored_label(
                ui.visuals().warn_fg_color,
                "Есть несохранённые изменения: они не участвуют в проверке.",
            );
        }
        let ready = snapshot.bot != BotStatus::Unconfigured || snapshot.local_only;
        if !ready {
            ui.label("Сохраните настройки, чтобы выполнить проверку.");
        }
        let retry = match &snapshot.diagnostics {
            DiagnosticStatus::Idle => {
                ui.weak("Проверка ещё не запускалась.");
                false
            }
            DiagnosticStatus::Running => {
                ui.horizontal_wrapped(|ui| {
                    ui.add(ProgressRing::indeterminate().size(20.0));
                    ui.label("Проверка выполняется…");
                });
                return;
            }
            DiagnosticStatus::Complete(report) => {
                diagnostic_report(ui, report, snapshot.local_only)
            }
        };
        let label = if retry {
            "Повторить проверку"
        } else {
            "Проверить настройки"
        };
        if ui.add_enabled(ready, Button::new(label).primary()).clicked() {
            send(Command::Diagnose);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_diagnostics_skip_telegram_and_use_the_real_cli_result() {
        let report = Report {
            workspace: WorkspaceCheck::Ready,
            agent: AgentCheck::Ready("1.2".into()),
            agent_kind: BackendKind::Codex,
            telegram: TelegramDiagnostic {
                token: DiagnosticStep::NotChecked,
                chat: DiagnosticStep::NotChecked,
                chat_kind: None,
                manage_topics: None,
            },
        };
        let ctx = egui::Context::default();
        let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
            assert!(!diagnostic_report(ui, &report, true));
        });
        output.textures_delta.clear();
        assert!(output.shapes.iter().any(|shape| matches!(&shape.shape, egui::Shape::Text(text)
            if text.galley.text() == "Telegram пропущен в локальном режиме.")));
    }

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
        assert!(labels.contains(&"Повторить проверку"));
        assert!(labels.contains(&"Право управлять темами: не проверено."));
        assert!(labels.contains(&"Рабочий каталог: недоступен. Проверьте путь в настройках."));
    }
}
