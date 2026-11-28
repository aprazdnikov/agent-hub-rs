//! Interactive memory browser. All stores, snapshots and project bindings live in this window.
mod context;
mod data;
mod lifecycle;
mod records;
mod transfer;

use crate::widgets::{Wait, action, card, dialog, primary, title};
use context::ContextDraft;
use data::{Archive, Projects, Status, Store};
use eframe::egui;
use kylig_ui::{InfoBar, Severity};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Default, Clone, Copy, PartialEq, Eq)]
enum Tab {
    #[default]
    Records,
    Context,
    Tools,
}

#[derive(Default, Clone, Copy, PartialEq, Eq)]
enum Tool {
    #[default]
    Overview,
    Import,
    Reset,
    Bind,
}

#[derive(Clone)]
enum Change {
    Status(String, Status),
    Delete(String),
    Prune(String, BTreeSet<u32>),
    Policy(bool),
    Import(Vec<data::Entry>),
    Reset(BTreeSet<String>),
    Bind { source: String, share: bool },
}

enum Job {
    Refresh,
    Read(String, u32),
    Change(Change),
    Export,
    Context,
    CopyContext,
}

#[derive(Default)]
struct Browser {
    tab: Tab,
    tool: Tool,
    query: String,
    filter: Option<Status>,
    detail: Option<(String, u32)>,
    lookup: records::Lookup,
    record_tab: records::RecordTab,
    key: String,
    revision: u32,
    prune: BTreeSet<u32>,
    context: ContextDraft,
    keep: BTreeSet<String>,
    names: BTreeMap<String, String>,
    use_export: bool,
    source: String,
    share: bool,
    wait: Wait,
    pending: Option<Job>,
    confirm: Option<(String, Change)>,
    notice: Option<(Severity, String)>,
}

pub(crate) struct Memory {
    project: String,
    data: Projects,
    view: Browser,
}

impl Default for Memory {
    fn default() -> Self {
        Self { project: "agent-hub".into(), data: Projects::default(), view: Browser::default() }
    }
}

impl Memory {
    pub(crate) fn open_tools(&mut self) {
        self.view.tab = Tab::Tools;
    }

    pub(crate) fn open_project(&mut self, name: &str) {
        if !name.is_empty() && self.project != name {
            self.data.register(name);
            name.clone_into(&mut self.project);
            self.view = Browser::default();
        }
    }
    pub(crate) fn show(&mut self, ui: &mut egui::Ui) {
        title(ui, "Память проекта", "Записи агентов, история ревизий и передача контекста.");
        let mut selected = self.project.clone();
        egui::ComboBox::from_label("Проект").selected_text(&selected).show_ui(ui, |ui| {
            for name in self.data.bindings.keys() {
                ui.selectable_value(&mut selected, name.clone(), name);
            }
        });
        self.open_project(&selected);
        if self.view.wait.draw(ui)
            && let Some(job) = self.view.pending.take()
        {
            let result = self.finish(job, ui);
            self.view.notice = Some(match result {
                Ok(message) => (Severity::Success, message),
                Err(error) => (Severity::Error, error),
            });
        }
        if let Some((severity, message)) = &self.view.notice {
            let mut open = true;
            InfoBar::new(*severity, message).open(&mut open).show(ui);
            if !open {
                self.view.notice = None;
            }
        }
        let Some(store) = self.data.store(&self.project) else {
            ui.label("Память перенесена в другой проект. Выберите новый каталог из списка.");
            return;
        };
        ui.add_enabled_ui(!self.view.wait.busy(), |ui| {
            ui.horizontal_wrapped(|ui| {
                for (tab, label) in [
                    (Tab::Records, "Записи"),
                    (Tab::Context, "Пакет контекста"),
                    (Tab::Tools, "Управление"),
                ] {
                    ui.selectable_value(&mut self.view.tab, tab, label);
                }
            });
            ui.separator();
            match self.view.tab {
                Tab::Records => self.view.records(ui, store),
                Tab::Context => self.view.context_panel(ui, store, &self.project),
                Tab::Tools => self.view.tools(ui, store, &self.data, &self.project),
            }
        });
        self.view.confirmation(ui);
    }

    fn finish(&mut self, job: Job, ui: &egui::Ui) -> Result<String, String> {
        let storage = self.data.storage_key(&self.project).ok_or("Проект недоступен.")?.to_owned();
        let store = self.data.stores.get_mut(&storage).ok_or("Хранилище недоступно.")?;
        match job {
            Job::Refresh => {
                Ok(format!("Список обновлён. Записей в проекте — {}.", store.entries.len()))
            }
            Job::Read(key, revision) => {
                let entry = store.entry(key.trim()).ok_or("Запись с таким ключом не найдена.")?;
                let revision = if revision == 0 { entry.latest() } else { revision };
                if !entry.revisions.contains_key(&revision) {
                    return Err("Такой ревизии нет в истории записи.".into());
                }
                self.view.detail = Some((entry.key.clone(), revision));
                self.view.record_tab = records::RecordTab::Text;
                self.view.prune.clear();
                Ok(format!("Открыта запись {} · r{revision}", entry.key))
            }
            Job::Context => {
                let packet = self.view.context.packet(store, &self.project)?;
                self.view.context.preview = Some(packet);
                Ok("Пакет готов к проверке.".into())
            }
            Job::CopyContext => {
                let packet = self.view.context.packet(store, &self.project)?;
                if self.view.context.preview.as_ref() != Some(&packet) {
                    return Err("Состав или поручение изменились. Соберите пакет заново.".into());
                }
                ui.ctx().copy_text(packet);
                Ok("Проверенный пакет скопирован.".into())
            }
            Job::Export => {
                self.data.archive =
                    Some(Archive { from: self.project.clone(), entries: store.entries.clone() });
                Ok(format!(
                    "Снимок сохранён в демо. Записей — {}. Откройте «Импорт», чтобы восстановить его.",
                    store.entries.len()
                ))
            }
            Job::Change(change) => self.apply(change, &storage),
        }
    }
}

impl Browser {
    fn start(&mut self, ui: &egui::Ui, job: Job, label: &str) {
        self.pending = Some(job);
        self.notice = None;
        self.wait.start(ui, label);
    }
    fn confirmation(&mut self, ui: &egui::Ui) {
        let Some((message, change)) = self.confirm.clone() else { return };
        let mut open = true;
        let (heading, accept, description) = match &change {
            Change::Policy(true) => (
                "Включить автоочистку?",
                "Включить",
                "Старые ревизии будут очищаться автоматически. Последняя ревизия каждой записи сохранится.",
            ),
            Change::Policy(false) => (
                "Отключить автоочистку?",
                "Перейти на ручную очистку",
                "Старые ревизии будут храниться до ручной очистки.",
            ),
            _ => ("Подтвердить изменение?", "Подтвердить", message.as_str()),
        };
        let confirmed =
            dialog(ui, "demo-memory-confirm", &mut open, heading).show(ui.ctx(), |ui| {
                if matches!(change, Change::Policy(_)) {
                    ui.label(&message);
                }
                ui.label(description);
                ui.add_space(8.0);
                ui.horizontal_wrapped(|ui| {
                    if action(ui, "Отмена") {
                        ui.close();
                    }
                    primary(ui, accept)
                })
                .inner
            });
        if confirmed == Some(true) {
            self.confirm = None;
            self.start(ui, Job::Change(change), "Применяем изменения…");
        } else if !open {
            self.confirm = None;
        }
    }
}

#[cfg(test)]
mod tests;
