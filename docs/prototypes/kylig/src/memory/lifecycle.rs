use super::{
    Archive, Browser, Change, Job, Memory, Projects, Store, Tool, action, card, egui, primary,
};
use kylig_ui::{Checkbox, Severity, TextField};

impl Browser {
    pub(super) fn tools(
        &mut self,
        ui: &mut egui::Ui,
        store: &Store,
        projects: &Projects,
        project: &str,
    ) {
        let identity = projects.storage_key(project);
        let linked: Vec<_> = projects
            .bindings
            .iter()
            .filter_map(|(name, key)| (key.as_deref() == identity).then_some(name.as_str()))
            .collect();
        card(ui, |ui| {
            ui.strong(format!("Хранилище · поколение {}", store.generation));
            ui.label(format!(
                "Записей — {} / 500   ·   ревизий — {}",
                store.entries.len(),
                store.entries.iter().map(|e| e.revisions.len()).sum::<usize>()
            ));
            ui.label(format!("Привязанные проекты   {}", linked.join(", ")));
            ui.label(if store.automatic {
                "Очистка истории автоматическая"
            } else {
                "Очистка истории вручную"
            });
        });
        ui.horizontal_wrapped(|ui| {
            if action(ui, "Экспорт") {
                self.start(ui, Job::Export, "Создаём снимок памяти…");
            }
            for (tool, label) in [
                (Tool::Import, "Импорт"),
                (Tool::Reset, "Новое поколение"),
                (Tool::Bind, "Привязка"),
                (Tool::Overview, "Очистка"),
            ] {
                if ui.selectable_value(&mut self.tool, tool, label).changed() && tool == Tool::Reset
                {
                    self.keep = store.entries.iter().map(|entry| entry.key.clone()).collect();
                }
            }
        });
        ui.add_space(8.0);
        match self.tool {
            Tool::Overview => {
                ui.strong("Политика очистки");
                ui.label("При автоочистке старые ревизии удаляются без отдельного подтверждения. Последняя ревизия каждой записи сохраняется.");
                let label = if store.automatic {
                    "Перейти на ручную очистку"
                } else {
                    "Включить автоочистку"
                };
                if action(ui, label) {
                    self.confirm =
                        Some((format!("Проект · {project}"), Change::Policy(!store.automatic)));
                }
            }
            Tool::Import => self.import_panel(ui, store, projects.archive.as_ref()),
            Tool::Reset => {
                ui.strong("Записи для нового поколения");
                ui.label("Перед сменой поколения демо сохранит снимок в разделе импорта. Выбранные записи останутся в памяти.");
                for entry in &store.entries {
                    let mut selected = self.keep.contains(&entry.key);
                    if ui.add(Checkbox::new(&mut selected, entry.title)).changed() {
                        if selected {
                            self.keep.insert(entry.key.clone());
                        } else {
                            self.keep.remove(&entry.key);
                        }
                    }
                }
                if primary(ui, "Проверить новое поколение") {
                    self.confirm = Some((
                        format!(
                            "Создать новое поколение? Останется записей — {} из {}. Потребуются новые агентские сессии.",
                            self.keep.len(),
                            store.entries.len()
                        ),
                        Change::Reset(self.keep.clone()),
                    ));
                }
            }
            Tool::Bind => self.binding_panel(ui, projects, project),
        }
    }

    fn binding_panel(&mut self, ui: &mut egui::Ui, projects: &Projects, project: &str) {
        ui.strong("Перенос проекта и общая память");
        ui.label(format!("Назначение   /demo/projects/{project}"));
        let width = ui.available_width().min(420.0);
        ui.add(
            TextField::new(&mut self.source)
                .label("Исходный каталог")
                .placeholder("/demo/projects/agent-hub")
                .width(width),
        );
        ui.horizontal_wrapped(|ui| {
            ui.selectable_value(&mut self.share, false, "Перенести привязку");
            ui.selectable_value(&mut self.share, true, "Общая память");
        });
        if primary(ui, "Проверить привязку") {
            let Some(source) = self
                .source
                .trim()
                .strip_prefix("/demo/projects/")
                .filter(|s| !s.is_empty() && !s.contains('/'))
            else {
                self.notice = Some((
                    Severity::Error,
                    "Укажите путь демо-проекта вида /demo/projects/agent-hub.".into(),
                ));
                return;
            };
            let error = if projects.storage_key(source).is_none() {
                Some("Исходный проект недоступен.")
            } else if projects.storage_key(source) == projects.storage_key(project) {
                Some("Эти пути уже используют одно хранилище.")
            } else if projects.store(source).is_none_or(|s| s.entries.is_empty()) {
                Some("В исходном проекте пока нет памяти.")
            } else if projects.store(project).is_some_and(|s| !s.entries.is_empty()) {
                Some(
                    "В проекте назначения уже есть записи. Для примера выберите пустой проект sandbox.",
                )
            } else {
                None
            };
            if let Some(message) = error {
                self.notice = Some((Severity::Error, message.into()));
            } else {
                let message = if self.share {
                    format!(
                        "Подключить {project} к общей памяти {source}? Оба проекта будут видеть одни записи."
                    )
                } else {
                    format!(
                        "Перенести память из {source} в {project}? Старый каталог потеряет привязку."
                    )
                };
                self.confirm =
                    Some((message, Change::Bind { source: source.into(), share: self.share }));
            }
        }
    }
}

impl Memory {
    pub(super) fn apply(&mut self, change: Change, storage: &str) -> Result<String, String> {
        let store = self.data.stores.get_mut(storage).ok_or("Хранилище недоступно.")?;
        let message = match change {
            Change::Status(key, status) => {
                let entry =
                    store.entries.iter_mut().find(|e| e.key == key).ok_or("Запись недоступна.")?;
                entry.status = status;
                format!("Статус {key} изменён на «{}».", status.label())
            }
            Change::Delete(key) => {
                store.entries.retain(|e| e.key != key);
                self.view.detail = None;
                self.view.context.selected.remove(&key);
                format!("Запись {key} удалена.")
            }
            Change::Prune(key, removed) => {
                let entry =
                    store.entries.iter_mut().find(|e| e.key == key).ok_or("Запись недоступна.")?;
                let latest = entry.latest();
                entry
                    .revisions
                    .retain(|version, _| *version == latest || !removed.contains(version));
                self.view.detail = Some((key.clone(), latest));
                self.view.prune.clear();
                if self.view.context.selected.get(&key).is_some_and(|r| removed.contains(r)) {
                    self.view.context.selected.remove(&key);
                }
                format!("История {key} сокращена. Последняя ревизия — r{latest}.")
            }
            Change::Policy(automatic) => {
                store.automatic = automatic;
                "Политика очистки сохранена.".into()
            }
            Change::Reset(keep) => {
                self.data.archive =
                    Some(Archive { from: self.project.clone(), entries: store.entries.clone() });
                store.entries.retain(|entry| keep.contains(&entry.key));
                store.generation += 1;
                self.view.detail = None;
                self.view.context = super::ContextDraft::default();
                self.view.tool = Tool::Overview;
                format!(
                    "Создано поколение {}. Снимок предыдущего поколения доступен для импорта.",
                    store.generation
                )
            }
            Change::Import(entries) => {
                let count = entries.len();
                store.entries.extend(entries);
                self.view.names.clear();
                self.view.tool = Tool::Overview;
                format!("Импорт завершён. Добавлено записей — {count}.")
            }
            Change::Bind { source, share } => {
                let id =
                    self.data.storage_key(&source).ok_or("Исходный проект недоступен.")?.to_owned();
                self.data.bindings.insert(self.project.clone(), Some(id));
                if !share {
                    self.data.bindings.insert(source, None);
                }
                self.view.context = super::ContextDraft::default();
                self.view.detail = None;
                self.view.tool = Tool::Overview;
                format!("Привязка памяти для {} обновлена.", self.project)
            }
        };
        self.view.context.preview = None;
        Ok(message)
    }
}
