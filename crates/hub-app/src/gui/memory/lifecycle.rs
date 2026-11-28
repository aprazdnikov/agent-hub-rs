use super::{
    Action, Button, MemoryOverview, Panel, Path, PathBuf, Receiver, Request, egui, mpsc, request,
    theme,
};
use hub_memory::{AdminAction, AdminPreview, BindingMode, CleanupPolicy, ImportChoice, ImportKind};
use hub_telegram::memory::lifecycle::{AUTO_WARNING, binding_text, status_name};
use kylig_ui::{Checkbox, TextField};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Copy)]
enum Picker {
    Export,
    Import,
    Reset,
}
type PickResult = (u64, Picker, Option<PathBuf>);

#[derive(Clone, Copy, Default, PartialEq)]
enum Tool {
    #[default]
    Cleanup,
    Import,
    Reset,
    Bind,
}

struct Confirmation {
    title: &'static str,
    accept: &'static str,
    body: String,
    action: Action,
}

#[derive(Default)]
pub(super) struct AdminUi {
    tool: Tool,
    panel: Option<Panel>,
    confirm: Option<Confirmation>,
    binding_source: String,
    binding_mode: BindingMode,
    picking: Option<Receiver<PickResult>>,
    names: BTreeMap<String, String>,
    excluded: BTreeSet<String>,
}
impl AdminUi {
    pub(super) fn open_import(&mut self) {
        self.tool = Tool::Import;
    }
    pub(super) fn picking(&self) -> bool {
        self.picking.is_some()
    }
    pub(super) fn completed(&mut self) {
        self.panel = None;
        self.confirm = None;
        self.names.clear();
        self.excluded.clear();
        self.tool = Tool::Cleanup;
    }

    pub(super) fn receive(&mut self, panel: Panel) {
        if matches!(panel, Panel::Reset { .. }) {
            self.excluded.clear();
        }
        if let Panel::Binding(preview) = panel {
            self.confirm = Some(Confirmation {
                title: "Подтвердить привязку памяти?",
                accept: "Подтвердить",
                body: binding_text(&preview),
                action: Action::ApplyBinding(preview),
            });
        } else {
            self.panel = Some(panel);
        }
    }

    pub(super) fn confirm_preview(&mut self, preview: AdminPreview) {
        let (title, accept, description) = match &preview.action {
            AdminAction::SetPolicy(CleanupPolicy::Automatic) => {
                ("Включить автоочистку?", "Включить", AUTO_WARNING.to_owned())
            }
            AdminAction::SetPolicy(CleanupPolicy::Manual) => (
                "Отключить автоочистку?",
                "Перейти на ручную очистку",
                "Старые ревизии будут храниться до ручной очистки.".into(),
            ),
            AdminAction::SetStatus { key, status } => (
                "Изменить статус записи?",
                "Подтвердить",
                format!("Запись {key} получит статус «{}».", status_name(*status)),
            ),
            AdminAction::Delete { keys } => (
                "Удалить записи и их историю?",
                "Удалить",
                format!(
                    "Ключи: {}. Для восстановления понадобится ранее сохранённый экспорт.",
                    keys.join(", ")
                ),
            ),
            AdminAction::Prune { revisions } => (
                "Удалить выбранные ревизии?",
                "Удалить",
                format!(
                    "Будет сокращена история записей: {}. Последняя ревизия каждого ключа сохранится.",
                    revisions
                        .iter()
                        .map(|(key, revs)| format!(
                            "{key}: {}",
                            revs.iter().map(|v| format!("r{v}")).collect::<Vec<_>>().join(", ")
                        ))
                        .collect::<Vec<_>>()
                        .join("; ")
                ),
            ),
        };
        self.confirm = Some(Confirmation {
            title,
            accept,
            body: format!(
                "{description}\n\nУдаляемых ревизий — {} · ключей — {}\nРазмер до изменения — {} байт, после — {} байт",
                preview.removed_revisions,
                preview.removed_keys.len(),
                preview.before_bytes,
                preview.after_bytes
            ),
            action: Action::Apply(preview),
        });
    }

    pub(super) fn confirmation(&mut self, ui: &egui::Ui) -> Option<Action> {
        let confirm = self.confirm.as_ref()?;
        let mut open = true;
        let answer =
            theme::dialog(ui, "memory-confirm", &mut open, confirm.title).show(ui.ctx(), |ui| {
                egui::ScrollArea::vertical()
                    .max_height((ui.ctx().content_rect().height() - 210.0).max(60.0))
                    .show(ui, |ui| {
                        ui.label(&confirm.body);
                    });
                ui.add_space(8.0);
                ui.horizontal_wrapped(|ui| {
                    if ui.add(Button::new("Отмена")).clicked() {
                        ui.close();
                    }
                    ui.add(Button::new(confirm.accept).primary()).clicked()
                })
                .inner
            });
        if answer == Some(true) {
            return self.confirm.take().map(|c| c.action);
        }
        if !open {
            self.confirm = None;
        }
        None
    }

    pub(super) fn poll_picker(&mut self, ui: &egui::Ui, generation: u64) -> Option<Request> {
        let picker = self.picking.as_ref()?;
        match picker.try_recv() {
            Ok((stamp, purpose, path)) => {
                self.picking = None;
                if stamp != generation {
                    return None;
                }
                path.map(|path| {
                    request(match purpose {
                        Picker::Export => Action::Export(path),
                        Picker::Import => Action::ImportFile(path),
                        Picker::Reset => Action::PrepareReset(path),
                    })
                })
            }
            Err(mpsc::TryRecvError::Disconnected) => {
                self.picking = None;
                None
            }
            Err(mpsc::TryRecvError::Empty) => {
                ui.ctx().request_repaint_after(std::time::Duration::from_millis(100));
                None
            }
        }
    }
    fn pick(&mut self, ui: &egui::Ui, generation: u64, purpose: Picker) {
        let (tx, rx) = mpsc::channel();
        self.picking = Some(rx);
        let ctx = ui.ctx().clone();
        std::thread::spawn(move || {
            let dialog = rfd::FileDialog::new().add_filter("Экспорт памяти JSON", &["json"]);
            let path = match purpose {
                Picker::Import => dialog.pick_file(),
                Picker::Export | Picker::Reset => dialog
                    .set_title("Новый файл экспорта (существующий не перезаписывается)")
                    .set_file_name("memory-export.json")
                    .save_file(),
            };
            let _ = tx.send((generation, purpose, path));
            ctx.request_repaint();
        });
    }

    pub(super) fn show(
        &mut self,
        ui: &mut egui::Ui,
        generation: u64,
        project: &Path,
        overview: Option<&MemoryOverview>,
        last_export: Option<&Path>,
    ) -> Option<Request> {
        theme::card(ui).show(ui, |ui| {
            if let Some(overview) = overview {
                ui.strong(format!("Хранилище · поколение {}", overview.token.generation));
                ui.label(format!(
                    "Записей — {} / 500   ·   ревизий — {}",
                    overview.page.key_count, overview.revision_count
                ));
                ui.label(format!(
                    "Привязанные проекты   {}",
                    overview
                        .page
                        .bound_projects
                        .iter()
                        .map(|p| p.display().to_string())
                        .collect::<Vec<_>>()
                        .join(", ")
                ));
                ui.label(match overview.policy {
                    CleanupPolicy::Manual => "Очистка истории вручную",
                    CleanupPolicy::Automatic => "Очистка истории автоматическая",
                });
                ui.weak(format!(
                    "{} / 8388608 байт   ·   отметок удаления — {} / 4096",
                    overview.page.bytes, overview.tombstone_count
                ));
                if overview.page.bound_projects.len() > 1 {
                    ui.weak("Запись, очистка и удаление затрагивают все привязанные проекты.");
                }
                if let Some(last) = &overview.last_cleanup {
                    ui.small(format!(
                        "Последняя очистка: удалено {} ревизий, Unix time {}",
                        last.removed_revisions, last.performed_at
                    ));
                }
            } else {
                ui.strong("Хранилище проекта");
                ui.label(project.display().to_string());
            }
        });
        ui.horizontal_wrapped(|ui| {
            if ui.add(Button::new("Экспорт…")).clicked() {
                self.pick(ui, generation, Picker::Export);
            }
            for (tool, label) in [
                (Tool::Import, "Импорт"),
                (Tool::Reset, "Новое поколение"),
                (Tool::Bind, "Привязка"),
                (Tool::Cleanup, "Очистка"),
            ] {
                if ui.selectable_value(&mut self.tool, tool, label).changed() {
                    self.panel = None;
                    self.excluded.clear();
                    self.names.clear();
                }
            }
        });
        ui.add_space(8.0);
        match self.tool {
            Tool::Cleanup => Self::cleanup(ui, overview),
            Tool::Bind => self.binding(ui, project),
            Tool::Import => self.import_file(ui, generation, last_export),
            Tool::Reset => {
                self.reset_generation(ui, generation, project);
                None
            }
        }
    }
    fn cleanup(ui: &mut egui::Ui, overview: Option<&MemoryOverview>) -> Option<Request> {
        let mut next = None;
        ui.strong("Политика очистки");
        ui.label("При автоочистке старые ревизии удаляются автоматически. Последняя ревизия каждой записи сохраняется.");
        if let Some(overview) = overview {
            let (label, target) = match overview.policy {
                CleanupPolicy::Automatic => ("Перейти на ручную очистку", CleanupPolicy::Manual),
                CleanupPolicy::Manual => ("Включить автоочистку", CleanupPolicy::Automatic),
            };
            if ui.add(Button::new(label)).clicked() {
                next = Some(request(Action::Preview(AdminAction::SetPolicy(target))));
            }
        }
        next
    }
    fn binding(&mut self, ui: &mut egui::Ui, project: &Path) -> Option<Request> {
        let mut next = None;
        ui.strong("Перенос проекта и общая память");
        ui.label(format!("Назначение   {}", project.display()));
        ui.add(
            TextField::new(&mut self.binding_source)
                .label("Исходный каталог")
                .placeholder("Абсолютный путь к прежнему проекту или worktree")
                .width(ui.available_width().min(420.0)),
        );
        ui.horizontal_wrapped(|ui| {
            ui.selectable_value(&mut self.binding_mode, BindingMode::Move, "Перенести привязку");
            ui.selectable_value(
                &mut self.binding_mode,
                BindingMode::Share,
                "Подключить общую память",
            );
        });
        if ui
            .add_enabled(
                !self.binding_source.trim().is_empty(),
                Button::new("Проверить привязку").primary(),
            )
            .clicked()
        {
            next = Some(request(Action::Bind {
                source: self.binding_source.trim().into(),
                mode: self.binding_mode,
            }));
        }
        next
    }
    fn reset_generation(&mut self, ui: &mut egui::Ui, generation: u64, project: &Path) {
        ui.strong("Записи для нового поколения");
        ui.label("Сначала сохраните экспорт в новый файл. Затем выберите записи, которые останутся в памяти.");
        if ui.add(Button::new("Сохранить снимок…")).clicked() {
            self.panel = None;
            self.pick(ui, generation, Picker::Reset);
        }
        if let Some(Panel::Reset { token, keep, receipt }) = &self.panel {
            ui.weak(format!("Проверенный экспорт: {}", receipt.path().display()));
            for key in keep {
                let mut selected = !self.excluded.contains(key);
                if ui.add(Checkbox::new(&mut selected, key)).changed() {
                    if selected {
                        self.excluded.remove(key);
                    } else {
                        self.excluded.insert(key.clone());
                    }
                }
            }
            if ui.add(Button::new("Проверить новое поколение").primary()).clicked()
            {
                let selected: Vec<_> =
                    keep.iter().filter(|k| !self.excluded.contains(*k)).cloned().collect();
                self.confirm = Some(Confirmation {
                    title: "Создать новое поколение?",
                    accept: "Создать поколение",
                    body: format!(
                        "Проект: {}\nОстанется записей: {} из {}. Остальные записи и отметки удаления будут удалены. Потребуются новые агентские сессии.",
                        project.display(),
                        selected.len(),
                        keep.len()
                    ),
                    action: Action::Reset {
                        token: token.clone(),
                        keep: selected,
                        receipt: receipt.clone(),
                    },
                });
            }
        }
    }
    fn import_file(
        &mut self,
        ui: &mut egui::Ui,
        generation: u64,
        last_export: Option<&Path>,
    ) -> Option<Request> {
        let mut next = None;
        ui.strong("Предпросмотр импорта");
        ui.horizontal_wrapped(|ui| {
            if ui.add(Button::new("Выбрать файл экспорта…")).clicked() {
                self.names.clear();
                self.panel = None;
                self.pick(ui, generation, Picker::Import);
            }
            if let Some(file) = last_export
                && ui
                    .add(Button::new("Последний снимок"))
                    .on_hover_text(file.display().to_string())
                    .clicked()
            {
                self.names.clear();
                self.panel = None;
                next = Some(request(Action::ImportFile(file.to_owned())));
            }
        });
        next.or_else(|| self.import_preview(ui))
    }
    fn import_preview(&mut self, ui: &mut egui::Ui) -> Option<Request> {
        let mut next = None;
        match self.panel.as_ref() {
            Some(Panel::Import { preview, export, choices }) => {
                let mut updated = choices.clone();
                let mut dirty = false;
                ui.weak(format!(
                    "Источник   {}   ·   записей — {}",
                    preview.source.display(),
                    preview.items.len()
                ));
                for item in &preview.items {
                    theme::card(ui).show(ui, |ui| {
                        import_item(ui, preview, item);
                        if ui.add(Button::new("Сравнить версии")).clicked() {
                            next = Some(request(Action::Compare {
                                preview: preview.clone(),
                                key: item.key.clone(),
                                local_revision: None,
                                incoming_revision: None,
                            }));
                        }
                        if matches!(item.kind, ImportKind::Conflict | ImportKind::Copied(_)) {
                            let name =
                                self.names.entry(item.key.clone()).or_insert_with(|| match choices
                                    .get(&item.key)
                                {
                                    Some(ImportChoice::CopyAs(name)) => name.clone(),
                                    _ => String::new(),
                                });
                            ui.add(
                                TextField::new(name)
                                    .id_salt(&item.key)
                                    .label("Ключ копии")
                                    .placeholder("Пусто — оставить локальную запись")
                                    .width(ui.available_width().min(360.0)),
                            );
                            let previous = match choices.get(&item.key) {
                                Some(ImportChoice::CopyAs(previous)) => previous,
                                _ => "",
                            };
                            if name.trim() == previous {
                                return;
                            }
                            dirty = true;
                            if name.trim().is_empty() {
                                updated.remove(&item.key);
                            } else {
                                updated.insert(
                                    item.key.clone(),
                                    ImportChoice::CopyAs(name.trim().to_owned()),
                                );
                            }
                        }
                    });
                }
                let body = format!(
                    "Источник: {}\nЦелевой проект: {}\nРазмер после импорта: {} байт. Политика автоочистки не импортируется.",
                    preview.source.display(),
                    preview.target.display(),
                    preview.after_bytes
                );
                ui.label(&body);
                if dirty {
                    if ui.add(Button::new("Проверить решения импорта").primary()).clicked()
                    {
                        next = Some(request(Action::Import {
                            export: export.clone(),
                            choices: updated,
                        }));
                    }
                } else if ui.add(Button::new("Импортировать в проект…").primary()).clicked()
                {
                    self.confirm = Some(Confirmation {
                        title: "Импортировать записи?",
                        accept: "Импортировать",
                        body,
                        action: Action::ApplyImport(preview.clone()),
                    });
                }
            }
            Some(Panel::Comparison { preview, key, local_revision, incoming_revision }) => {
                next =
                    Self::comparison_panel(ui, preview, key, *local_revision, *incoming_revision)
                        .or(next);
            }
            _ => {}
        }
        next
    }
    fn comparison_panel(
        ui: &mut egui::Ui,
        preview: &hub_memory::ImportPreview,
        key: &str,
        local_revision: Option<u32>,
        incoming_revision: Option<u32>,
    ) -> Option<Request> {
        let mut action = None;
        match preview.conflict(key) {
            Ok(conflict) => {
                comparison_columns(ui, &conflict, |ui, record, local| {
                    let selected = if local { local_revision } else { incoming_revision };
                    ui.label(hub_telegram::memory::lifecycle::record_text(record, selected));
                    ui.horizontal_wrapped(|ui| {
                        for entry in record.history.iter().rev() {
                            let selected =
                                selected.or_else(|| record.history.last().map(|e| e.revision));
                            if ui
                                .selectable_label(
                                    selected == Some(entry.revision),
                                    format!("r{}", entry.revision),
                                )
                                .clicked()
                            {
                                action = Some(request(Action::Compare {
                                    preview: preview.clone(),
                                    key: key.into(),
                                    local_revision: if local {
                                        Some(entry.revision)
                                    } else {
                                        local_revision
                                    },
                                    incoming_revision: if local {
                                        incoming_revision
                                    } else {
                                        Some(entry.revision)
                                    },
                                }));
                            }
                        }
                    });
                });
            }
            Err(error) => {
                ui.label(error.to_string());
            }
        }
        if ui.add(Button::new("Вернуться к решениям импорта")).clicked() {
            action = Some(request(Action::ReopenImport(preview.clone())));
        }
        action
    }
}

fn import_item(
    ui: &mut egui::Ui,
    preview: &hub_memory::ImportPreview,
    item: &hub_memory::ImportItem,
) {
    let versions = preview.conflict(&item.key).ok();
    if let Some(entry) = versions.as_ref().and_then(|v| v.incoming.history.last()) {
        ui.strong(&entry.title);
    }
    ui.monospace(&item.key);
    ui.label(hub_telegram::memory::lifecycle::import_name(&item.kind));
    if matches!(item.kind, ImportKind::Conflict | ImportKind::Copied(_))
        && let Some(versions) = &versions
    {
        comparison_columns(ui, versions, |ui, record, _| {
            if record.history.is_empty() {
                ui.label(hub_telegram::memory::lifecycle::record_text(record, None));
            } else {
                for entry in record.history.iter().rev() {
                    ui.weak(format!("Ревизия {}", entry.revision));
                    ui.label(&entry.body);
                }
            }
        });
    }
}

fn comparison_columns(
    ui: &mut egui::Ui,
    versions: &hub_memory::ImportConflict<'_>,
    mut body: impl FnMut(&mut egui::Ui, &hub_memory::ImportRecord<'_>, bool),
) {
    let columns = if ui.available_width() >= 620.0 { 2 } else { 1 };
    for row in [
        ("В памяти проекта", &versions.local, true),
        ("В импортируемом файле", &versions.incoming, false),
    ]
    .chunks(columns)
    {
        ui.columns(columns, |slots| {
            for (slot, &(label, record, local)) in slots.iter_mut().zip(row) {
                egui::Frame::new()
                    .fill(slot.visuals().extreme_bg_color)
                    .inner_margin(12)
                    .corner_radius(6)
                    .show(slot, |ui| {
                        ui.set_width(ui.available_width());
                        ui.strong(label);
                        body(ui, record, local);
                    });
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn stale_file_picker_result_cannot_import_into_new_project() {
        let mut state = AdminUi::default();
        let (send, receive) = mpsc::channel();
        state.picking = Some(receive);
        send.send((1, Picker::Import, Some(PathBuf::from("old.json")))).unwrap();
        let context = egui::Context::default();
        let mut request = None;
        let mut output = context.run_ui(egui::RawInput::default(), |ui| {
            request = state.poll_picker(ui, 2);
        });
        output.textures_delta.clear();
        assert!(request.is_none());
        assert!(state.picking.is_none());
    }
}
