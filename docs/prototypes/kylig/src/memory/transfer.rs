use super::{Archive, Browser, Change, Store, card, egui, primary};
use kylig_ui::{Severity, TextField};
use std::collections::BTreeSet;

impl Browser {
    pub(super) fn import_panel(
        &mut self,
        ui: &mut egui::Ui,
        store: &Store,
        snapshot: Option<&Archive>,
    ) {
        ui.strong("Предпросмотр импорта");
        ui.horizontal_wrapped(|ui| {
            if ui.selectable_value(&mut self.use_export, false, "Образец экспорта").changed()
            {
                self.names.clear();
            }
            if snapshot.is_some()
                && ui.selectable_value(&mut self.use_export, true, "Последний снимок").changed()
            {
                self.names.clear();
            }
        });
        let fixture = Archive::default();
        let archive = if self.use_export { snapshot.unwrap_or(&fixture) } else { &fixture };
        ui.weak(format!("Источник   {}   ·   записей — {}", archive.from, archive.entries.len()));
        let mut additions = Vec::new();
        let mut occupied: BTreeSet<_> = store.entries.iter().map(|e| e.key.clone()).collect();
        // Reserve incoming keys as well so conflict resolutions cannot shadow a later new record.
        occupied.extend(archive.entries.iter().map(|e| e.key.clone()));
        let mut invalid = None;
        for entry in &archive.entries {
            let local = store.entry(&entry.key);
            card(ui, |ui| {
                ui.strong(entry.title);
                ui.monospace(&entry.key);
                match local {
                    None => {
                        ui.label("Новая запись");
                        additions.push(entry.clone());
                    }
                    Some(local) if local == entry => {
                        ui.label("Совпадает с локальной записью. Пропускается.");
                    }
                    Some(local) => {
                        ui.label("Конфликт ключей. Чтобы сохранить копию, укажите свободный ключ.");
                        compare(ui, local, entry);
                        let width = ui.available_width().min(360.0);
                        let name = self.names.entry(entry.key.clone()).or_default();
                        ui.add(
                            TextField::new(name)
                                .id_salt(&entry.key)
                                .label("Ключ копии")
                                .placeholder("Пусто — пропустить")
                                .width(width),
                        );
                        let name = name.trim();
                        if !name.is_empty() {
                            if name.len() > 64
                                || !name
                                    .bytes()
                                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
                            {
                                invalid = Some("Используйте латинские буквы, цифры и знаки _ - (до 64 символов).".to_owned());
                            } else if occupied.insert(name.to_owned()) {
                                let mut copy = entry.clone();
                                name.clone_into(&mut copy.key);
                                additions.push(copy);
                            } else {
                                invalid = Some(format!("Ключ «{name}» уже занят."));
                            }
                        }
                    }
                }
            });
        }
        if primary(ui, "Проверить импорт") {
            if let Some(message) = invalid {
                self.notice = Some((Severity::Error, message));
            } else if additions.is_empty() {
                self.notice = Some((
                    Severity::Info,
                    "Нет новых записей. Для конфликта можно указать ключ копии.".into(),
                ));
            } else {
                self.confirm = Some((
                    format!(
                        "Добавить в память записей — {}? Существующие ключи сохранятся.",
                        additions.len()
                    ),
                    Change::Import(additions),
                ));
            }
        }
    }
}

fn compare(ui: &mut egui::Ui, local: &super::data::Entry, incoming: &super::data::Entry) {
    let versions = [("В памяти проекта", local), ("В импортируемом файле", incoming)];
    let columns = if ui.available_width() >= 620.0 { 2 } else { 1 };
    for row in versions.chunks(columns) {
        ui.columns(columns, |slots| {
            for (slot, &(label, entry)) in slots.iter_mut().zip(row) {
                egui::Frame::new()
                    .fill(slot.visuals().extreme_bg_color)
                    .inner_margin(12)
                    .corner_radius(6)
                    .show(slot, |ui| {
                        ui.set_width(ui.available_width());
                        ui.strong(label);
                        for (revision, body) in entry.revisions.iter().rev() {
                            ui.weak(format!("Ревизия {revision}"));
                            ui.label(*body);
                        }
                    });
            }
        });
    }
}
