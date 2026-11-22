//! A revision-bound draft survives background reads; only an explicit save accepts it.
use eframe::egui;
use hub_core::tasks::ProjectRules;
use hub_telegram::hub::tasks::Request;

#[derive(Default)]
pub(super) struct Editor {
    pub saved: ProjectRules,
    draft: ProjectRules,
}
impl Editor {
    pub fn dirty(&self) -> bool {
        self.draft != self.saved
    }
    pub fn observe(&mut self, rules: ProjectRules) {
        if !self.dirty() {
            self.draft = rules.clone();
        }
        self.saved = rules;
    }
    pub fn accept(&mut self, rules: ProjectRules) {
        self.draft = rules.clone();
        self.saved = rules;
    }
    pub fn show(
        &mut self,
        ui: &mut egui::Ui,
        project: &std::path::Path,
        saving: bool,
        blocked: bool,
    ) -> Option<Request> {
        let mut request = None;
        ui.collapsing("Правила проекта", |ui| {
            ui.label(format!("Сохранённая редакция: {} · текстовые инструкции: {}", self.saved.revision,
                if self.saved.enabled { "Включены" } else { "Выключены" }));
            ui.small("Общие для всех задач этого каталога. Подготовка Git выполняется приложением отдельно, до первого запуска.");
            ui.add_enabled_ui(!saving, |ui| {
                ui.checkbox(&mut self.draft.enabled, "Добавлять правила к каждому запуску");
                ui.add(egui::TextEdit::multiline(&mut self.draft.text)
                    .id_salt(("project_rules_text", project)).desired_rows(4).desired_width(f32::INFINITY)
                    .hint_text("Например: перед новой задачей обновлять базовую ветку Dev и создавать рабочую ветку от неё.").char_limit(16384));
            });
            ui.add_enabled_ui(!saving, |ui| {
                let mut enabled = self.draft.git.is_some();
                if ui.checkbox(&mut enabled, "Требовать подготовку Git перед новой задачей").changed() {
                    self.draft.git = enabled.then(|| hub_core::tasks::git::GitPolicy { remote: "origin".into(), base: String::new() });
                }
                if let Some(git) = &mut self.draft.git {
                    ui.label("Remote (имя, не URL)");
                    ui.add(egui::TextEdit::singleline(&mut git.remote).id_salt(("git_remote", project)).char_limit(80));
                    ui.label("Базовая ветка — точное имя, например Dev или master");
                    ui.add(egui::TextEdit::singleline(&mut git.base).id_salt(("git_base", project)).char_limit(160));
                    ui.small("Перед новой задачей: чистый каталог → fetch → fast-forward базы → новая ветка. Без stash, reset и push.");
                }
            });
            if self.draft.revision != self.saved.revision {
                ui.label("Правила изменены в другой сессии. Ваш черновик сохранён на экране; скопируйте нужный текст и загрузите актуальную редакцию.");
            } else if self.dirty() {
                ui.label("Есть несохранённые изменения. Сохраните или загрузите сохранённую редакцию перед запуском.");
            }
            if let Err(error) = self.draft.validate() {
                ui.label(error);
            }
            ui.horizontal(|ui| {
                if ui.add_enabled(!blocked && self.dirty() && self.draft.revision == self.saved.revision && self.draft.validate().is_ok(), egui::Button::new("Сохранить правила")).clicked() {
                    request = Some(if self.draft.git == self.saved.git {
                        Request::SaveRules { revision: self.draft.revision, enabled: self.draft.enabled, text: self.draft.text.clone() }
                    } else { Request::SaveProjectRules { revision: self.draft.revision, rules: self.draft.clone() } });
                }
                if ui.add_enabled(!saving && self.dirty(), egui::Button::new("Загрузить сохранённую редакцию")).clicked() {
                    self.accept(self.saved.clone());
                }
            });
        });
        request
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn refresh_preserves_draft_and_conflict_until_explicit_acceptance() {
        let mut editor = Editor::default();
        editor.observe(ProjectRules { git: None, revision: 1, enabled: true, text: "Old".into() });
        editor.draft.text = "Typing".into();
        editor.observe(ProjectRules {
            git: None,
            revision: 2,
            enabled: false,
            text: "Elsewhere".into(),
        });
        assert_eq!(editor.draft.text, "Typing");
        assert_eq!(editor.draft.revision, 1);
        assert!(editor.dirty());
        editor.accept(editor.saved.clone());
        assert!(!editor.dirty());
        assert_eq!(editor.draft.text, "Elsewhere");
    }
}
