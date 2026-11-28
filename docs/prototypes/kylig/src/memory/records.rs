use super::{Browser, Change, Job, Status, Store, Tab, action, card, egui, primary};
use kylig_ui::{Checkbox, Chip, ListItem, Severity, TextField};

#[derive(Default, Clone, Copy, PartialEq, Eq)]
pub(super) enum Lookup {
    #[default]
    Search,
    Exact,
}

#[derive(Default, Clone, Copy, PartialEq, Eq)]
pub(super) enum RecordTab {
    #[default]
    Text,
    Manage,
}

impl Browser {
    pub(super) fn records(&mut self, ui: &mut egui::Ui, store: &Store) {
        if let Some((key, revision)) = self.detail.clone()
            && let Some(entry) = store.entry(&key)
        {
            self.detail_panel(ui, entry, revision);
            return;
        }
        kylig_ui::Segmented::new(
            &mut self.lookup,
            [(Lookup::Search, "Поиск записей"), (Lookup::Exact, "По ключу")],
        )
        .id_salt("memory-lookup-mode")
        .show(ui);
        if self.lookup == Lookup::Exact {
            self.exact_lookup(ui);
            return;
        }
        let width = ui.available_width().min(420.0);
        ui.add(
            TextField::new(&mut self.query)
                .id_salt("memory-search")
                .placeholder("Найти запись по ключу или названию")
                .clearable(true)
                .width(width),
        );
        ui.horizontal_wrapped(|ui| {
            egui::ComboBox::from_label("Статус")
                .selected_text(self.filter.map_or("Все записи", Status::label))
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut self.filter, None, "Все записи");
                    for status in Status::ALL {
                        ui.selectable_value(&mut self.filter, Some(status), status.label());
                    }
                });
            if action(ui, "Обновить") {
                self.start(ui, Job::Refresh, "Читаем память проекта…");
            }
        });
        let query = self.query.trim().to_lowercase();
        let matches: Vec<_> = store
            .entries
            .iter()
            .filter(|entry| {
                self.filter.is_none_or(|status| status == entry.status)
                    && (entry.key.to_lowercase().contains(&query)
                        || entry.title.to_lowercase().contains(&query))
            })
            .collect();
        ui.add_space(6.0);
        if store.entries.is_empty() {
            card(ui, |ui| {
                ui.strong("Хранилище пусто");
                ui.label("Загрузите пример экспорта, чтобы добавить первые записи.");
                if action(ui, "Перейти к импорту") {
                    self.tab = Tab::Tools;
                    self.tool = super::Tool::Import;
                }
            });
        } else if matches.is_empty() {
            ui.label("По этим условиям записей нет.");
            if action(ui, "Сбросить поиск и статус") {
                self.query.clear();
                self.filter = None;
            }
        } else {
            ui.weak(format!("Найдено записей — {}", matches.len()));
            card(ui, |ui| {
                for entry in matches {
                    let result = ListItem::new(entry.title)
                        .chevron(true)
                        .id_salt(&entry.key)
                        .subtitle(format!(
                            "{}  ·  r{}  ·  {}",
                            entry.key,
                            entry.latest(),
                            entry.status.label()
                        ))
                        .show(ui);
                    if result.response.clicked() {
                        self.start(ui, Job::Read(entry.key.clone(), 0), "Читаем историю записи…");
                    }
                }
            });
        }
    }

    fn exact_lookup(&mut self, ui: &mut egui::Ui) {
        card(ui, |ui| {
            let width = ui.available_width().min(320.0);
            ui.add(
                TextField::new(&mut self.key)
                    .label("Ключ записи")
                    .placeholder("release-check")
                    .width(width),
            );
            ui.horizontal_wrapped(|ui| {
                ui.label("Ревизия");
                ui.add(egui::DragValue::new(&mut self.revision).range(0..=u32::MAX));
                ui.weak("0 — последняя");
                if action(ui, "Открыть запись") {
                    self.start(ui, Job::Read(self.key.clone(), self.revision), "Читаем ревизию…");
                }
            });
        });
    }

    fn detail_panel(&mut self, ui: &mut egui::Ui, entry: &super::data::Entry, revision: u32) {
        if action(ui, "‹  К списку записей") {
            self.detail = None;
            self.notice = None;
            return;
        }
        kylig_ui::Segmented::new(
            &mut self.record_tab,
            [(RecordTab::Text, "Содержание"), (RecordTab::Manage, "Управление записью")],
        )
        .id_salt("memory-record-tab")
        .show(ui);
        if self.record_tab == RecordTab::Manage {
            self.manage_record(ui, entry);
            return;
        }
        card(ui, |ui| {
            Chip::new(entry.status.label()).show(ui);
            ui.heading(entry.title);
            ui.weak(format!("{}  ·  {}  ·  r{revision}", entry.key, entry.kind));
            ui.add_space(8.0);
            if let Some(body) = entry.revisions.get(&revision) {
                ui.label(*body);
            }
            ui.small(format!("Источник   {}", entry.source));
            ui.separator();
            ui.horizontal_wrapped(|ui| {
                ui.label("История ревизий");
                for &version in entry.revisions.keys().rev() {
                    if ui.selectable_label(version == revision, format!("r{version}")).clicked() {
                        self.detail = Some((entry.key.clone(), version));
                    }
                }
            });
        });
        let selected = self.context.selected.get(&entry.key) == Some(&revision);
        ui.horizontal_wrapped(|ui| {
            if selected {
                ui.weak("Эта ревизия в пакете");
                if action(ui, "Открыть пакет") {
                    self.tab = Tab::Context;
                }
            } else if primary(ui, "Добавить в пакет контекста") {
                self.context.selected.insert(entry.key.clone(), revision);
                self.context.preview = None;
                self.notice = Some((
                    Severity::Success,
                    format!("{} · r{revision} добавлена в пакет.", entry.key),
                ));
            }
        });
    }

    fn manage_record(&mut self, ui: &mut egui::Ui, entry: &super::data::Entry) {
        card(ui, |ui| {
            ui.strong(entry.title);
            ui.weak(format!("Текущий статус · {}", entry.status.label()));
            ui.horizontal_wrapped(|ui| {
                for (status, label) in [
                    (Status::Active, "Вернуть в активные"),
                    (Status::Stale, "Пометить устаревшей"),
                    (Status::Archived, "В архив"),
                ] {
                    if entry.status != status && action(ui, label) {
                        self.confirm = Some((
                            format!(
                                "Установить для «{}» статус «{}»?",
                                entry.title,
                                status.label()
                            ),
                            Change::Status(entry.key.clone(), status),
                        ));
                    }
                }
                if ui.add(kylig_ui::Button::new("Удалить запись").danger()).clicked() {
                    self.confirm = Some((
                        format!(
                            "Удалить «{}» и всю историю? Ревизий — {}.",
                            entry.title,
                            entry.revisions.len()
                        ),
                        Change::Delete(entry.key.clone()),
                    ));
                }
            });
            if entry.revisions.len() > 1 {
                ui.label("Удаление старых ревизий");
                ui.horizontal_wrapped(|ui| {
                    for &version in
                        entry.revisions.keys().filter(|&&version| version != entry.latest())
                    {
                        let mut checked = self.prune.contains(&version);
                        if ui.add(Checkbox::new(&mut checked, format!("r{version}"))).changed() {
                            if checked {
                                self.prune.insert(version);
                            } else {
                                self.prune.remove(&version);
                            }
                        }
                    }
                    if !self.prune.is_empty() && action(ui, "Удалить выбранные") {
                        self.confirm = Some((
                            format!(
                                "Удалить старые ревизии {:?} записи «{}»?",
                                self.prune, entry.title
                            ),
                            Change::Prune(entry.key.clone(), self.prune.clone()),
                        ));
                    }
                });
            }
        });
    }
}
