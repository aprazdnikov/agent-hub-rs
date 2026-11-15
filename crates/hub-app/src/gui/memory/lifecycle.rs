use super::{PathBuf, Receiver, Request, egui, entry_text, mpsc};
use hub_memory::{
    AdminAction, CleanupPolicy, ImportChoice, ImportKind, MemoryStatus, StatusFilter,
};
use hub_telegram::memory::lifecycle::{AUTO_WARNING, Action, Panel, preview_text};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Copy)]
enum Picker {
    Export,
    Import,
    Reset,
}
type PickResult = (u64, Picker, Option<PathBuf>);
#[derive(Default)]
pub(super) struct AdminUi {
    filter: StatusFilter,
    picking: Option<Receiver<PickResult>>,
    names: BTreeMap<String, String>,
    removed: BTreeSet<u32>,
    excluded: BTreeSet<String>,
}
fn request(action: Action) -> Request {
    Request::Admin(Box::new(action))
}
impl AdminUi {
    pub(super) fn clear(&mut self) {
        self.names.clear();
        self.removed.clear();
        self.excluded.clear();
    }
    pub(super) fn controls(
        &mut self,
        ui: &mut egui::Ui,
        generation: u64,
        enabled: bool,
        query: &str,
    ) -> Option<Request> {
        let mut action = None;
        if let Some(rx) = &self.picking {
            match rx.try_recv() {
                Ok((stamp, purpose, path)) => {
                    self.picking = None;
                    if stamp == generation
                        && let Some(path) = path
                    {
                        action = Some(request(match purpose {
                            Picker::Export => Action::Export(path),
                            Picker::Import => Action::ImportFile(path),
                            Picker::Reset => Action::PrepareReset(path),
                        }));
                    }
                }
                Err(mpsc::TryRecvError::Disconnected) => self.picking = None,
                Err(mpsc::TryRecvError::Empty) => {
                    ui.ctx().request_repaint_after(std::time::Duration::from_millis(100));
                }
            }
        }
        ui.add_enabled_ui(enabled, |ui| {
            ui.horizontal_wrapped(|ui| {
                egui::ComboBox::from_id_salt("memory_status_filter")
                    .selected_text(format!("{:?}", self.filter))
                    .show_ui(ui, |ui| {
                        for (value, label) in [
                            (StatusFilter::Active, "Активные"),
                            (StatusFilter::Stale, "Устаревшие"),
                            (StatusFilter::Archived, "Архив"),
                            (StatusFilter::All, "Все"),
                        ] {
                            ui.selectable_value(&mut self.filter, value, label);
                        }
                    });
                if ui.button("Показать состояние и записи").clicked() {
                    action = Some(request(Action::Overview {
                        filter: self.filter,
                        query: query.into(),
                        offset: 0,
                    }));
                }
                for (label, purpose) in [
                    ("Экспорт…", Picker::Export),
                    ("Импорт…", Picker::Import),
                    ("Новое поколение…", Picker::Reset),
                ] {
                    if ui.add_enabled(self.picking.is_none(), egui::Button::new(label)).clicked() {
                        let (tx, rx) = mpsc::channel();
                        self.picking = Some(rx);
                        std::thread::spawn(move || {
                            let dialog =
                                rfd::FileDialog::new().add_filter("Экспорт памяти JSON", &["json"]);
                            let path = match purpose {
                                Picker::Import => dialog.pick_file(),
                                Picker::Export | Picker::Reset => dialog
                                    .set_title(
                                        "Новый файл экспорта (существующий не перезаписывается)",
                                    )
                                    .set_file_name("memory-export.json")
                                    .save_file(),
                            };
                            let _ = tx.send((generation, purpose, path));
                        });
                    }
                }
            });
        });
        action
    }
    pub(super) fn panel(&mut self, ui: &mut egui::Ui, panel: &Panel) -> Option<Request> {
        let mut action = None;
        match panel {
            Panel::Comparison { preview, key, local_revision, incoming_revision } => {
                return Self::comparison_panel(
                    ui,
                    preview,
                    key,
                    *local_revision,
                    *incoming_revision,
                );
            }
            Panel::Notice(text) => {
                ui.label(text);
            }
            Panel::Overview(overview) => return self.overview_panel(ui, overview),
            Panel::Details(details) => return self.details_panel(ui, details),
            Panel::Confirm(preview) => {
                ui.label(preview_text(preview));
                let automatic =
                    matches!(preview.action, AdminAction::SetPolicy(CleanupPolicy::Automatic));
                if automatic {
                    ui.colored_label(ui.visuals().warn_fg_color, AUTO_WARNING);
                }
                if ui
                    .button(if automatic {
                        "Включить автоочистку"
                    } else if preview.removed_revisions > 0 {
                        "Подтвердить удаление (экспорт — отдельной кнопкой выше)"
                    } else {
                        "Подтвердить изменение"
                    })
                    .clicked()
                {
                    action = Some(request(Action::Apply(preview.clone())));
                }
                if ui.button("Отмена").clicked() {
                    action = Some(request(Action::Overview {
                        filter: self.filter,
                        query: String::new(),
                        offset: 0,
                    }));
                }
            }
            Panel::Import { preview, export, choices } => {
                return self.import_panel(ui, preview, export, choices);
            }
            Panel::Reset { token, keep, receipt } => {
                ui.label("Экспорт сохранён и проверен. Выберите записи для нового поколения. Остальные записи и отметки удаления будут удалены. Нужны новые агентские сессии.");
                for key in keep {
                    let mut selected = !self.excluded.contains(key);
                    if ui.checkbox(&mut selected, key).changed() {
                        if selected {
                            self.excluded.remove(key);
                        } else {
                            self.excluded.insert(key.clone());
                        }
                    }
                }
                if ui.button("Подтвердить новое поколение").clicked() {
                    action = Some(request(Action::Reset {
                        token: token.clone(),
                        keep: keep
                            .iter()
                            .filter(|key| !self.excluded.contains(*key))
                            .cloned()
                            .collect(),
                        receipt: receipt.clone(),
                    }));
                }
                if ui.button("Отмена пересоздания").clicked() {
                    action = Some(request(Action::Overview {
                        filter: self.filter,
                        query: String::new(),
                        offset: 0,
                    }));
                }
            }
        }
        action
    }
}

impl AdminUi {
    fn overview_panel(
        &mut self,
        ui: &mut egui::Ui,
        overview: &hub_memory::MemoryOverview,
    ) -> Option<Request> {
        let mut action = None;

        ui.label(format!("Проект: {}", overview.page.project.display()));
        ui.label(format!(
            "Очистка: {}. Ключи: {}/500 · отметки удаления: {}/4096 · {} / 8388608 байт",
            hub_telegram::memory::lifecycle::policy_name(overview.policy),
            overview.page.key_count,
            overview.tombstone_count,
            overview.page.bytes
        ));
        if let Some(last) = &overview.last_cleanup {
            ui.label(format!(
                "Последняя очистка: удалено {} ревизий, Unix time {}",
                last.removed_revisions, last.performed_at
            ));
        }
        ui.horizontal(|ui| {
            for (label, policy) in [
                ("Ручной режим (рекомендуется)", CleanupPolicy::Manual),
                ("Настроить автоочистку…", CleanupPolicy::Automatic),
            ] {
                if ui.button(label).clicked() {
                    action = Some(request(Action::Preview(AdminAction::SetPolicy(policy))));
                }
            }
        });
        for entry in &overview.page.entries {
            if ui.button(format!("{} · r{} · {}", entry.key, entry.revision, entry.title)).clicked()
            {
                self.removed.clear();
                action = Some(request(Action::Details { key: entry.key.clone(), revision: None }));
            }
        }
        ui.horizontal(|ui| {
            for (label, offset) in [
                ("←", overview.page.offset.checked_sub(20)),
                (
                    "→",
                    (overview.page.offset + 20 < overview.page.matched_count)
                        .then_some(overview.page.offset + 20),
                ),
            ] {
                if ui.add_enabled(offset.is_some(), egui::Button::new(label)).clicked()
                    && let Some(offset) = offset
                {
                    action = Some(request(Action::Overview {
                        filter: overview.filter,
                        query: overview.query.clone(),
                        offset,
                    }));
                }
            }
        });

        action
    }
    fn details_panel(
        &mut self,
        ui: &mut egui::Ui,
        details: &hub_memory::EntryDetails,
    ) -> Option<Request> {
        let mut action = None;

        ui.label(format!(
            "Состояние: {}. История сокращена: {}",
            hub_telegram::memory::lifecycle::status_name(details.status),
            details.history_pruned
        ));
        ui.label(entry_text(&details.entry));
        ui.horizontal_wrapped(|ui| {
            for revision in &details.revisions {
                if ui.button(format!("r{revision}")).clicked() {
                    action = Some(request(Action::Details {
                        key: details.entry.key.clone(),
                        revision: Some(*revision),
                    }));
                }
            }
        });
        ui.horizontal_wrapped(|ui| {
            for (label, status) in [
                ("В архив…", MemoryStatus::Archived),
                ("Пометить устаревшей…", MemoryStatus::Stale),
                ("Вернуть в активные…", MemoryStatus::Active),
            ] {
                if ui.button(label).clicked() {
                    action = Some(request(Action::Preview(AdminAction::SetStatus {
                        key: details.entry.key.clone(),
                        status,
                    })));
                }
            }
            if ui.button("Удалить запись…").clicked() {
                action = Some(request(Action::Preview(AdminAction::Delete {
                    keys: vec![details.entry.key.clone()],
                })));
            }
        });
        ui.collapsing("Удалить выбранные старые ревизии", |ui| {
            for revision in details.revisions.iter().take(details.revisions.len().saturating_sub(1))
            {
                let mut selected = self.removed.contains(revision);
                if ui.checkbox(&mut selected, format!("r{revision}")).changed() {
                    if selected {
                        self.removed.insert(*revision);
                    } else {
                        self.removed.remove(revision);
                    }
                }
            }
            if ui
                .add_enabled(!self.removed.is_empty(), egui::Button::new("Предпросмотр очистки…"))
                .clicked()
            {
                action = Some(request(Action::Preview(AdminAction::Prune {
                    revisions: BTreeMap::from([(
                        details.entry.key.clone(),
                        self.removed.iter().copied().collect(),
                    )]),
                })));
            }
        });

        action
    }
    fn import_panel(
        &mut self,
        ui: &mut egui::Ui,
        preview: &hub_memory::ImportPreview,
        export: &hub_memory::MemoryExport,
        choices: &BTreeMap<String, ImportChoice>,
    ) -> Option<Request> {
        let mut action = None;

        ui.label(format!(
            "Источник: {}\nЦелевой проект: {}\nРазмер после импорта: {} байт",
            preview.source.display(),
            preview.target.display(),
            preview.after_bytes
        ));
        ui.label("Конфликты пропускаются, пока вы не выберете новый ключ. Политика автоочистки не импортируется.");
        for item in &preview.items {
            ui.label(format!(
                "{}: {}",
                item.key,
                hub_telegram::memory::lifecycle::import_name(&item.kind)
            ));
            if ui.button(format!("Просмотреть обе версии: {}", item.key)).clicked()
            {
                action = Some(request(Action::Compare {
                    preview: preview.clone(),
                    key: item.key.clone(),
                    local_revision: None,
                    incoming_revision: None,
                }));
            }
            if item.kind == ImportKind::Conflict {
                let name = self.names.entry(item.key.clone()).or_insert_with(|| {
                    format!("{}-import", item.key.chars().take(57).collect::<String>())
                });
                ui.horizontal(|ui| {
                    ui.text_edit_singleline(name);
                    if ui.button("Сохранить как копию").clicked() {
                        let mut choices = choices.clone();
                        choices.insert(item.key.clone(), ImportChoice::CopyAs(name.clone()));
                        action = Some(request(Action::Import { export: export.clone(), choices }));
                    }
                });
            }
        }
        if ui.button("Подтвердить импорт в указанный проект").clicked()
        {
            action = Some(request(Action::ApplyImport(preview.clone())));
        }
        if ui.button("Отмена импорта").clicked() {
            action = Some(request(Action::Overview {
                filter: self.filter,
                query: String::new(),
                offset: 0,
            }));
        }

        action
    }
}

impl AdminUi {
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
                for (label, record, selected, local) in [
                    ("Локальная запись", &conflict.local, local_revision, true),
                    ("Входящая запись", &conflict.incoming, incoming_revision, false),
                ] {
                    ui.heading(label);
                    ui.label(hub_telegram::memory::lifecycle::record_text(record, selected));
                    ui.horizontal_wrapped(|ui| {
                        for entry in record.history {
                            if ui.button(format!("{label}: r{}", entry.revision)).clicked() {
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
                }
            }
            Err(error) => {
                ui.label(error.to_string());
            }
        }
        if ui.button("Вернуться к решениям импорта").clicked() {
            action = Some(request(Action::ReopenImport(preview.clone())));
        }
        action
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
            request = state.controls(ui, 2, true, "");
        });
        output.textures_delta.clear();
        assert!(request.is_none());
        assert!(state.picking.is_none());
    }
}
