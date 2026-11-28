use super::{
    Action, Button, EntryDetails, MemoryOverview, Request, StatusFilter, Tab,
    context::ContextDraft, egui, lifecycle::AdminUi, request, theme,
};
use hub_memory::{AdminAction, EntryKind, MemoryStatus};
use kylig_ui::{Checkbox, Chip, ListItem, Segmented, TextField};
use std::collections::{BTreeMap, BTreeSet};
use std::time::{Duration, Instant};

fn status_label(status: MemoryStatus) -> &'static str {
    match status {
        MemoryStatus::Active => "Активная",
        MemoryStatus::Stale => "Устаревшая",
        MemoryStatus::Archived => "В архиве",
    }
}

fn kind_label(kind: EntryKind) -> &'static str {
    match kind {
        EntryKind::Context => "Контекст",
        EntryKind::Decision => "Решение",
        EntryKind::Progress => "Ход работы",
        EntryKind::Artifact => "Артефакт",
    }
}

pub(super) struct Records {
    pub exact: bool,
    pub report_refresh: bool,
    manage: bool,
    query: String,
    edited: Option<Instant>,
    filter: StatusFilter,
    key: String,
    revision: u32,
    prune: BTreeSet<u32>,
}

impl Default for Records {
    fn default() -> Self {
        Self {
            exact: false,
            report_refresh: false,
            manage: false,
            query: String::new(),
            edited: None,
            filter: StatusFilter::All,
            key: String::new(),
            revision: 0,
            prune: BTreeSet::new(),
        }
    }
}

impl Records {
    pub(super) fn list(&self, offset: usize) -> Request {
        request(Action::Overview { query: self.query.clone(), filter: self.filter, offset })
    }

    fn reset_detail(&mut self) {
        self.manage = false;
        self.prune.clear();
    }

    pub(super) fn show(
        &mut self,
        ui: &mut egui::Ui,
        overview: Option<&MemoryOverview>,
        tab: &mut Tab,
        admin: &mut AdminUi,
    ) -> Option<Request> {
        Segmented::new(&mut self.exact, [(false, "Поиск записей"), (true, "По ключу")])
            .id_salt("memory-lookup")
            .show(ui);
        if self.exact {
            return self.exact_lookup(ui);
        }
        let mut next = self.search(ui);
        let Some(overview) = overview.filter(|o| o.filter == self.filter && o.query == self.query)
        else {
            return next;
        };
        let page = &overview.page;
        ui.add_space(6.0);
        if page.key_count == 0 {
            theme::card(ui).show(ui, |ui| {
                ui.strong("Хранилище пусто");
                ui.label("Агенты сохраняют записи во время работы. Также можно импортировать экспорт памяти.");
                if ui.add(Button::new("Перейти к импорту")).clicked() {
                    *tab = Tab::Manage;
                    admin.open_import();
                }
            });
            return next;
        }
        if page.entries.is_empty() {
            ui.label("По этим условиям записей нет.");
            if ui.add(Button::new("Сбросить поиск и статус")).clicked() {
                self.query.clear();
                self.filter = StatusFilter::All;
                next = Some(self.list(0));
            }
            return next;
        }
        ui.weak(format!("Найдено записей — {}", page.matched_count))
            .on_hover_text(page.directory.display().to_string());
        theme::card(ui).show(ui, |ui| {
            for entry in &page.entries {
                let mut subtitle = format!("{}  ·  r{}", entry.key, entry.revision);
                if let Some(&status) = overview.statuses.get(&entry.key) {
                    subtitle.push_str("  ·  ");
                    subtitle.push_str(status_label(status));
                }
                if ListItem::new(&entry.title)
                    .id_salt(&entry.key)
                    .subtitle(subtitle)
                    .chevron(true)
                    .show(ui)
                    .response
                    .clicked()
                {
                    self.reset_detail();
                    next =
                        Some(request(Action::Details { key: entry.key.clone(), revision: None }));
                }
            }
        });
        if page.offset > 0 || page.offset + page.entries.len() < page.matched_count {
            ui.horizontal_wrapped(|ui| {
                if ui.add_enabled(page.offset > 0, Button::new("Назад")).clicked() {
                    next = Some(self.list(page.offset.saturating_sub(20)));
                }
                if ui
                    .add_enabled(
                        page.offset + page.entries.len() < page.matched_count,
                        Button::new("Далее"),
                    )
                    .clicked()
                {
                    next = Some(self.list(page.offset + 20));
                }
            });
        }
        next
    }

    fn exact_lookup(&mut self, ui: &mut egui::Ui) -> Option<Request> {
        theme::card(ui)
            .show(ui, |ui| {
                if ui
                    .add(
                        TextField::new(&mut self.key)
                            .label("Ключ записи")
                            .placeholder("release-check")
                            .width(ui.available_width().min(320.0)),
                    )
                    .changed()
                {
                    self.revision = 0;
                }
                ui.horizontal_wrapped(|ui| {
                    ui.label("Ревизия");
                    ui.add(egui::DragValue::new(&mut self.revision).range(0..=u32::MAX));
                    ui.weak("0 — последняя");
                    ui.add_enabled(!self.key.trim().is_empty(), Button::new("Открыть запись"))
                        .clicked()
                        .then(|| {
                            self.reset_detail();
                            Request::Get {
                                key: self.key.trim().to_owned(),
                                revision: (self.revision > 0).then_some(self.revision),
                            }
                        })
                })
                .inner
            })
            .inner
    }

    fn search(&mut self, ui: &mut egui::Ui) -> Option<Request> {
        let search = ui.add(
            TextField::new(&mut self.query)
                .id_salt("memory-search")
                .placeholder("Найти запись по ключу или названию")
                .clearable(true)
                .width(ui.available_width().min(420.0)),
        );
        if search.changed() {
            self.edited = Some(Instant::now());
        }
        let mut next = None;
        ui.horizontal_wrapped(|ui| {
            let filters = [
                (StatusFilter::All, "Все записи"),
                (StatusFilter::Active, status_label(MemoryStatus::Active)),
                (StatusFilter::Stale, status_label(MemoryStatus::Stale)),
                (StatusFilter::Archived, status_label(MemoryStatus::Archived)),
            ];
            let label = filters
                .iter()
                .find(|(f, _)| *f == self.filter)
                .map_or("Все записи", |(_, label)| *label);
            egui::ComboBox::from_label("Статус").selected_text(label).show_ui(ui, |ui| {
                for (filter, label) in filters {
                    if ui.selectable_value(&mut self.filter, filter, label).changed() {
                        next = Some(self.list(0));
                    }
                }
            });
            if ui.add(Button::new("Обновить")).clicked() {
                self.report_refresh = true;
                next = Some(self.list(0));
            }
        });
        if let Some(edited) = self.edited {
            let delay = Duration::from_millis(250);
            if edited.elapsed() >= delay && ui.is_enabled() {
                next = Some(self.list(0));
                self.edited = None;
            } else {
                ui.ctx().request_repaint_after(delay.saturating_sub(edited.elapsed()));
            }
        }
        next
    }

    pub(super) fn detail(
        &mut self,
        ui: &mut egui::Ui,
        details: &EntryDetails,
        context: &mut ContextDraft,
        tab: &mut Tab,
        notice: &mut Option<String>,
    ) -> Option<Request> {
        Segmented::new(&mut self.manage, [(false, "Содержание"), (true, "Управление записью")])
            .id_salt("memory-record-tab")
            .show(ui);
        let entry = &details.entry;
        let mut next = None;
        if self.manage {
            return theme::card(ui).show(ui, |ui| self.manage_record(ui, details)).inner;
        }
        theme::card(ui).show(ui, |ui| {
            Chip::new(status_label(details.status)).show(ui);
            ui.heading(&entry.title);
            ui.weak(format!(
                "{}  ·  {}  ·  r{}",
                entry.key,
                kind_label(entry.kind),
                entry.revision
            ));
            ui.add_space(8.0);
            ui.label(&entry.body);
            ui.small(format!("Автор   {}  ·  Unix time {}", entry.author, entry.created_at));
            ui.small(format!("Источники\n{}", entry.sources.join("\n")));
            ui.small("Автор и источники — метаданные, не подтверждение одобрения человеком.");
            ui.separator();
            if details.history_pruned {
                ui.weak("Часть старых ревизий удалена.");
            }
            ui.horizontal_wrapped(|ui| {
                ui.label("История ревизий");
                for &version in details.revisions.iter().rev() {
                    if ui
                        .selectable_label(version == entry.revision, format!("r{version}"))
                        .clicked()
                        && version != entry.revision
                    {
                        next = Some(request(Action::Details {
                            key: entry.key.clone(),
                            revision: Some(version),
                        }));
                    }
                }
            });
        });
        if context.select(ui, entry, tab) {
            *notice = Some(format!("{} · r{} добавлена в пакет.", entry.key, entry.revision));
        }
        next
    }

    fn manage_record(&mut self, ui: &mut egui::Ui, details: &EntryDetails) -> Option<Request> {
        self.prune.retain(|revision| details.revisions.contains(revision));
        let mut action = None;
        let key = &details.entry.key;
        ui.strong(&details.entry.title);
        ui.weak(format!("Текущий статус · {}", status_label(details.status)));
        ui.horizontal_wrapped(|ui| {
            for (label, status) in [
                ("Вернуть в активные", MemoryStatus::Active),
                ("Пометить устаревшей", MemoryStatus::Stale),
                ("В архив", MemoryStatus::Archived),
            ] {
                if status != details.status && ui.add(Button::new(label)).clicked() {
                    action = Some(AdminAction::SetStatus { key: key.clone(), status });
                }
            }
            if ui.add(Button::new("Удалить запись").danger()).clicked() {
                action = Some(AdminAction::Delete { keys: vec![key.clone()] });
            }
        });
        if details.revisions.len() > 1 {
            ui.label("Удаление старых ревизий");
            ui.horizontal_wrapped(|ui| {
                for &revision in details.revisions.iter().take(details.revisions.len() - 1) {
                    let mut checked = self.prune.contains(&revision);
                    if ui.add(Checkbox::new(&mut checked, format!("r{revision}"))).changed() {
                        if checked {
                            self.prune.insert(revision);
                        } else {
                            self.prune.remove(&revision);
                        }
                    }
                }
                if !self.prune.is_empty() && ui.add(Button::new("Удалить выбранные")).clicked()
                {
                    action = Some(AdminAction::Prune {
                        revisions: BTreeMap::from([(
                            key.clone(),
                            self.prune.iter().copied().collect(),
                        )]),
                    });
                }
            });
        }
        action.map(|a| request(Action::Preview(a)))
    }
}
