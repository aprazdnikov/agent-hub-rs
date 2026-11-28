//! Reusable commands and a local preview of their Telegram buttons.

use super::projects::title;
use super::{
    ActionDraft, Button, Draft, Field, FieldError, MAX_ACTIONS, ProjectDraft, SettingsForm, egui,
    input, messages, path_field, pick_folder, text_field, theme,
};
use kylig_ui::{Chip, ListItem, Segmented, TextArea};

#[derive(Default, PartialEq, Eq, Clone, Copy)]
enum Tab {
    #[default]
    Edit,
    Menu,
}

enum Playback {
    Playing { until: f64 },
    Stopped,
    Complete,
}

struct Preview {
    action: ActionDraft,
    playback: Playback,
}

#[derive(Default)]
pub(super) struct Editor {
    tab: Tab,
    selected: Option<String>,
    preview: Option<Preview>,
}

impl Editor {
    pub(super) fn edit(&mut self, id: Option<String>) {
        self.tab = Tab::Edit;
        self.selected = id;
    }

    fn list(&mut self, ui: &mut egui::Ui, actions: &[ActionDraft]) {
        for item in actions {
            if ListItem::new(title(&item.name, "Новое действие"))
                .id_salt(&item.id)
                .icon(kylig_icons::TERMINAL_WINDOW)
                .subtitle(title(item.command.lines().next().unwrap_or_default(), "Введите команду"))
                .selected(self.selected.as_ref() == Some(&item.id))
                .chevron(true)
                .show(ui)
                .response
                .clicked()
            {
                self.edit(Some(item.id.clone()));
            }
        }
    }

    fn menu(&mut self, ui: &mut egui::Ui, actions: &[ActionDraft]) {
        let now = ui.input(|input| input.time);
        if let Some(preview) = &mut self.preview
            && matches!(preview.playback, Playback::Playing {until} if now >= until)
        {
            preview.playback = Playback::Complete;
        }
        theme::card(ui).show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            ui.heading("Предпросмотр Telegram");
            ui.weak("Кнопки из текущих настроек. Запуск и остановка моделируются без выполнения команды.");
            if actions.is_empty() {
                ui.label("В меню пока нет команд.");
            }
            for item in actions {
                self.preview_button(ui, item, now);
            }
            if let Some(preview) = &self.preview {
                ui.separator();
                ui.strong(match preview.playback {
                    Playback::Playing { .. } => "Предпросмотр запуска",
                    Playback::Stopped => "Предпросмотр остановлен",
                    Playback::Complete => "Предпросмотр завершён",
                });
                ui.label(title(&preview.action.name, "Новое действие"));
                ui.weak(format!("Рабочий каталог · {}", title(&preview.action.cwd, "~")));
                ui.monospace(&preview.action.command);
            }
        });
    }

    fn preview_button(&mut self, ui: &mut egui::Ui, item: &ActionDraft, now: f64) {
        const DURATION: std::time::Duration = std::time::Duration::from_millis(1350);
        let active =
            self.preview.as_ref().filter(|p| matches!(p.playback, Playback::Playing { .. }));
        let label = title(&item.name, "Новое действие");
        if active.is_some_and(|preview| preview.action.id == item.id) {
            if ui
                .add(Button::new(format!("Остановить · {label}")).icon(kylig_icons::STOP))
                .clicked()
                && let Some(preview) = &mut self.preview
            {
                preview.playback = Playback::Stopped;
            }
        } else {
            let available =
                active.is_none() && !item.name.trim().is_empty() && !item.command.trim().is_empty();
            if ui.add_enabled(available, Button::new(label).icon(kylig_icons::PLAY)).clicked() {
                self.preview = Some(Preview {
                    action: item.clone(),
                    playback: Playback::Playing { until: now + DURATION.as_secs_f64() },
                });
                ui.ctx().request_repaint_after(DURATION);
            }
        }
    }

    fn detail(&mut self, ui: &mut egui::Ui, draft: &mut Draft) {
        let Some(action) = draft.actions.iter_mut().find(|a| self.selected.as_ref() == Some(&a.id))
        else {
            return;
        };
        let id = action.id.clone();
        let remove = ui
            .push_id(&id, |ui| {
                theme::card(ui).show(ui, |ui| action_fields(ui, action, &mut draft.projects)).inner
            })
            .inner;
        if remove {
            draft.actions.retain(|a| a.id != id);
            for project in &mut draft.projects {
                project.actions.retain(|a| a != &id);
            }
            self.selected = None;
        }
    }
}

impl SettingsForm {
    pub(super) fn actions(&mut self, ui: &mut egui::Ui, errors: &[FieldError]) {
        let editor = &mut self.actions;
        ui.horizontal_wrapped(|ui| {
            Segmented::new(&mut editor.tab, [(Tab::Edit, "Команды"), (Tab::Menu, "Меню /actions")])
                .id_salt("actions-view")
                .show(ui);
            if editor.tab == Tab::Edit
                && ui
                    .add_enabled(
                        self.draft.actions.len() < MAX_ACTIONS,
                        Button::new("Добавить действие").icon(kylig_icons::PLUS),
                    )
                    .clicked()
            {
                let id = uuid::Uuid::new_v4().simple().to_string();
                self.draft.actions.push(ActionDraft { id: id.clone(), ..Default::default() });
                editor.edit(Some(id));
            }
        });
        ui.add_space(6.0);
        messages(ui, errors, Field::Actions);
        if editor.tab == Tab::Menu {
            editor.menu(ui, &self.draft.actions);
            return;
        }
        if !self.draft.actions.iter().any(|a| Some(&a.id) == editor.selected.as_ref()) {
            editor.selected = None;
        }
        let Some(first) = self.draft.actions.first() else {
            ui.weak("Пока нет действий. Добавьте первую команду.");
            return;
        };
        if ui.available_width() >= 680.0 {
            editor.selected.get_or_insert_with(|| first.id.clone());
            let list_width = (ui.available_width() * 0.32).min(320.0);
            ui.horizontal_top(|ui| {
                ui.vertical(|ui| {
                    ui.set_width(list_width);
                    egui::ScrollArea::vertical()
                        .id_salt("action-list")
                        .max_height(theme::remaining_height(ui).max(80.0))
                        .show(ui, |ui| editor.list(ui, &self.draft.actions));
                });
                ui.vertical(|ui| {
                    ui.set_width(ui.available_width());
                    editor.detail(ui, &mut self.draft);
                });
            });
        } else if editor.selected.is_some() {
            if ui.link("‹ Все команды").clicked() {
                editor.selected = None;
            }
            editor.detail(ui, &mut self.draft);
        } else {
            editor.list(ui, &self.draft.actions);
        }
    }
}

fn action_fields(
    ui: &mut egui::Ui,
    action: &mut ActionDraft,
    projects: &mut [ProjectDraft],
) -> bool {
    let width = ui.available_width();
    ui.set_min_width(width);
    text_field(
        ui,
        "Название действия",
        input(&mut action.name).placeholder("Название кнопки в Telegram"),
    );
    ui.add(
        TextArea::new(&mut action.command).id_salt("command").label("Команда").rows(3).width(width),
    );
    path_field(ui, "Рабочий каталог", &mut action.cwd, "Домашний каталог", pick_folder);
    ui.strong("Связанные проекты");
    if projects.is_empty() {
        ui.weak("Профилей проектов пока нет.");
    }
    ui.horizontal_wrapped(|ui| {
        for project in projects {
            let linked = project.actions.contains(&action.id);
            if Chip::new(title(&project.name, "Новый проект"))
                .selected(linked)
                .show(ui)
                .response
                .clicked()
            {
                project.actions.retain(|id| id != &action.id);
                if !linked {
                    project.actions.push(action.id.clone());
                }
            }
        }
    });
    ui.separator();
    ui.small(if cfg!(windows) {
        "Оболочка: cmd.exe /D /S /C"
    } else {
        "Оболочка: /bin/sh -c"
    });
    ui.small("Telegram запускает команду от вашего пользователя без ввода пароля и отправляет вывод в группу. Остановка бота завершает запущенные команды.");
    ui.add(Button::new("Убрать действие").ghost()).clicked()
}
