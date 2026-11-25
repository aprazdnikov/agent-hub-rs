//! A revision-bound draft survives background reads; only an explicit save accepts it.
use eframe::egui;
use hub_core::{
    domain::BackendKind,
    settings::AgentRoles,
    tasks::{
        ProjectRules,
        workflow::{WorkflowChoice, WorkflowPreset},
    },
};
use hub_telegram::hub::tasks::Request;

/// Which of the three states the project check block is in; derived from the draft, never stored.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Off,
    Inherit,
    Custom,
}

impl Mode {
    const ALL: [Self; 3] = [Self::Off, Self::Inherit, Self::Custom];
    const fn label(self) -> &'static str {
        match self {
            Self::Off => "Выключено",
            Self::Inherit => "Наследовать общие роли",
            Self::Custom => "Свой выбор",
        }
    }
    const fn of(workflow: Option<&WorkflowChoice>) -> Self {
        match workflow {
            None => Self::Off,
            Some(WorkflowChoice::Inherit { .. }) => Self::Inherit,
            Some(WorkflowChoice::Custom(_)) => Self::Custom,
        }
    }
    /// The choice this mode creates; a command the project already typed survives the switch.
    fn chosen(self, previous: Option<&WorkflowChoice>) -> Option<WorkflowChoice> {
        let command = previous.map(WorkflowChoice::command).unwrap_or_default().to_owned();
        match self {
            Self::Off => None,
            Self::Inherit => Some(WorkflowChoice::Inherit { command }),
            Self::Custom => Some(WorkflowChoice::Custom(WorkflowPreset {
                implementer: BackendKind::Codex,
                command,
                reviewer: BackendKind::Claude,
            })),
        }
    }
}

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
    fn workflow_settings(
        &mut self,
        ui: &mut egui::Ui,
        project: &std::path::Path,
        roles: AgentRoles,
    ) {
        ui.label("Исправить и проверить автоматически");
        let current = Mode::of(self.draft.workflow.as_ref());
        let mut picked = current;
        ui.horizontal_wrapped(|ui| {
            for mode in Mode::ALL {
                ui.selectable_value(&mut picked, mode, mode.label());
            }
        });
        if picked != current {
            self.draft.workflow = picked.chosen(self.draft.workflow.as_ref());
        }
        match &mut self.draft.workflow {
            None => {}
            Some(WorkflowChoice::Inherit { command }) => {
                ui.small(format!(
                    "Исполнитель: {} · Ревьюер: {} (общие роли)",
                    roles.implementer.name(),
                    roles.reviewer.name()
                ));
                ui.label("Доверенная команда проверки");
                ui.add(
                    egui::TextEdit::singleline(command)
                        .id_salt(("workflow_command", project))
                        .hint_text("cargo test")
                        .char_limit(2048),
                );
                ui.small("Ревьюер меняется для новых запусков сразу после правки общих ролей; запущенная попытка доигрывает старую конфигурацию.");
            }
            Some(WorkflowChoice::Custom(plan)) => {
                ui.small("Применяется к запускам выбранного исполнителя из GUI и Telegram. После ошибки проверки допускается одна попытка исправления.");
                for (label, backend) in
                    [("Исполнитель", &mut plan.implementer), ("Ревьюер", &mut plan.reviewer)]
                {
                    egui::ComboBox::from_id_salt((project, label))
                        .selected_text(format!("{label}: {}", backend.name()))
                        .show_ui(ui, |ui| {
                            for candidate in BackendKind::ALL {
                                ui.selectable_value(backend, candidate, candidate.name());
                            }
                        });
                }
                ui.label("Доверенная команда проверки на этом компьютере");
                ui.add(
                    egui::TextEdit::singleline(&mut plan.command)
                        .hint_text("cargo test")
                        .char_limit(2048),
                );
            }
        }
    }
    pub fn show(
        &mut self,
        ui: &mut egui::Ui,
        project: &std::path::Path,
        roles: AgentRoles,
        saving: bool,
        blocked: bool,
    ) -> Option<Request> {
        let mut request = None;
        ui.label(format!(
            "Сохранённая редакция: {} · текстовые инструкции: {}",
            self.saved.revision,
            if self.saved.enabled { "Включены" } else { "Выключены" }
        ));
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
                    ui.small("Перед новой задачей: чистый каталог › fetch › fast-forward базы › новая ветка. Без stash, reset и push.");
                }
            });
        ui.add_enabled_ui(!saving, |ui| self.workflow_settings(ui, project, roles));
        if self.draft.revision != self.saved.revision {
            ui.label("Правила изменены в другой сессии. Ваш черновик сохранён на экране; скопируйте нужный текст и загрузите актуальную редакцию.");
        } else if self.dirty() {
            ui.label("Есть несохранённые изменения. Сохраните или загрузите сохранённую редакцию перед запуском.");
        }
        if let Err(error) = self.draft.validate() {
            ui.label(error);
        }
        ui.horizontal(|ui| {
            if ui
                .add_enabled(
                    !blocked
                        && self.dirty()
                        && self.draft.revision == self.saved.revision
                        && self.draft.validate().is_ok(),
                    egui::Button::new("Сохранить правила"),
                )
                .clicked()
            {
                request = Some(
                    if self.draft.git == self.saved.git
                        && self.draft.workflow == self.saved.workflow
                    {
                        Request::SaveRules {
                            revision: self.draft.revision,
                            enabled: self.draft.enabled,
                            text: self.draft.text.clone(),
                        }
                    } else {
                        Request::SaveProjectRules {
                            revision: self.draft.revision,
                            rules: self.draft.clone(),
                        }
                    },
                );
            }
            if ui
                .add_enabled(
                    !saving && self.dirty(),
                    egui::Button::new("Загрузить сохранённую редакцию"),
                )
                .clicked()
            {
                self.accept(self.saved.clone());
            }
        });
        request
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    const PROJECT: &str = "/tmp/agent-hub-rules";

    fn render(
        ctx: &egui::Context,
        editor: &mut Editor,
        roles: AgentRoles,
        events: Vec<egui::Event>,
        requests: &mut Vec<Request>,
    ) -> egui::FullOutput {
        let mut output = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(960.0, 900.0),
                )),
                events,
                ..Default::default()
            },
            |ui| {
                egui::CentralPanel::default().frame(egui::Frame::NONE).show(ui, |ui| {
                    if let Some(request) = editor.show(ui, Path::new(PROJECT), roles, false, false)
                    {
                        requests.push(request);
                    }
                });
            },
        );
        output.textures_delta.clear();
        output
    }

    fn has_text(output: &egui::FullOutput, label: &str) -> bool {
        output.shapes.iter().any(
            |shape| matches!(&shape.shape, egui::Shape::Text(text) if text.galley.text() == label),
        )
    }

    fn click(
        ctx: &egui::Context,
        editor: &mut Editor,
        roles: AgentRoles,
        label: &str,
        requests: &mut Vec<Request>,
    ) {
        render(ctx, editor, roles, Vec::new(), requests);
        let output = render(ctx, editor, roles, Vec::new(), requests);
        let position = output
            .shapes
            .iter()
            .find_map(|shape| match &shape.shape {
                egui::Shape::Text(text) if text.galley.text() == label => {
                    Some(text.galley.rect.translate(text.pos.to_vec2()).center())
                }
                _ => None,
            })
            .unwrap_or_else(|| panic!("missing label: {label}"));
        for pressed in [true, false] {
            render(
                ctx,
                editor,
                roles,
                vec![
                    egui::Event::PointerMoved(position),
                    egui::Event::PointerButton {
                        pos: position,
                        button: egui::PointerButton::Primary,
                        pressed,
                        modifiers: egui::Modifiers::NONE,
                    },
                ],
                requests,
            );
        }
    }

    fn rules(workflow: Option<WorkflowChoice>) -> ProjectRules {
        ProjectRules {
            revision: 1, enabled: true, text: "Правила".into(), git: None, workflow
        }
    }

    #[test]
    fn workflow_modes_offer_three_states_and_inherit_reports_shared_roles() {
        let ctx = egui::Context::default();
        ctx.all_styles_mut(|style| style.animation_time = 0.0);
        let roles = AgentRoles { implementer: BackendKind::Qwen, reviewer: BackendKind::Hermes };
        let mut editor = Editor::default();
        editor.observe(rules(None));
        let mut requests = Vec::new();
        let output = render(&ctx, &mut editor, roles, Vec::new(), &mut requests);
        for state in ["Выключено", "Наследовать общие роли", "Свой выбор"]
        {
            assert!(has_text(&output, state), "missing segment {state}");
        }
        assert!(!has_text(&output, "Доверенная команда проверки"));
        assert!(!editor.dirty());
        click(&ctx, &mut editor, roles, "Наследовать общие роли", &mut requests);
        assert_eq!(editor.draft, rules(Some(WorkflowChoice::Inherit { command: String::new() })));
        assert!(editor.dirty());
        let output = render(&ctx, &mut editor, roles, Vec::new(), &mut requests);
        assert!(
            has_text(&output, "Исполнитель: qwen · Ревьюер: hermes (общие роли)"),
            "the fact row must name the shared roles"
        );
        assert!(has_text(&output, "Доверенная команда проверки"));
        // An empty command is invalid: the save button stays blocked.
        click(&ctx, &mut editor, roles, "Сохранить правила", &mut requests);
        assert!(requests.is_empty(), "an empty command must not save");
        editor.draft = rules(Some(WorkflowChoice::Inherit { command: "cargo test".into() }));
        click(&ctx, &mut editor, roles, "Сохранить правила", &mut requests);
        assert!(
            matches!(
                requests.as_slice(),
                [Request::SaveProjectRules { rules, .. }]
                    if rules.workflow == Some(WorkflowChoice::Inherit { command: "cargo test".into() })
            ),
            "the inherited mode must reach the request"
        );
    }

    #[test]
    fn workflow_modes_fact_row_follows_the_settings_without_a_resave() {
        let ctx = egui::Context::default();
        ctx.all_styles_mut(|style| style.animation_time = 0.0);
        let mut editor = Editor::default();
        editor.observe(rules(Some(WorkflowChoice::Inherit { command: "cargo test".into() })));
        let mut requests = Vec::new();
        let moved =
            AgentRoles { implementer: BackendKind::Opencode, reviewer: BackendKind::Claude };
        let output = render(&ctx, &mut editor, moved, Vec::new(), &mut requests);
        assert!(
            has_text(&output, "Исполнитель: opencode · Ревьюер: claude (общие роли)"),
            "editing the shared roles must show up in the rules editor at once"
        );
        assert!(!editor.dirty(), "the project needs no resave");
    }

    #[test]
    fn workflow_modes_custom_keeps_both_combos_and_off_hides_the_block() {
        let ctx = egui::Context::default();
        ctx.all_styles_mut(|style| style.animation_time = 0.0);
        let roles = AgentRoles { implementer: BackendKind::Qwen, reviewer: BackendKind::Hermes };
        let mut editor = Editor::default();
        editor.observe(rules(Some(WorkflowChoice::Inherit { command: "cargo test".into() })));
        let mut requests = Vec::new();
        click(&ctx, &mut editor, roles, "Свой выбор", &mut requests);
        assert_eq!(
            editor.draft,
            rules(Some(WorkflowChoice::Custom(WorkflowPreset {
                implementer: BackendKind::Codex,
                command: "cargo test".into(),
                reviewer: BackendKind::Claude,
            })))
        );
        let output = render(&ctx, &mut editor, roles, Vec::new(), &mut requests);
        assert!(has_text(&output, "Исполнитель: codex"));
        assert!(has_text(&output, "Ревьюер: claude"));
        assert!(!has_text(&output, "Исполнитель: qwen · Ревьюер: hermes (общие роли)"));
        click(&ctx, &mut editor, roles, "Выключено", &mut requests);
        assert_eq!(editor.draft, rules(None));
        let output = render(&ctx, &mut editor, roles, Vec::new(), &mut requests);
        assert!(!has_text(&output, "Доверенная команда проверки"));
        assert!(!has_text(&output, "Исполнитель: codex"));
    }

    #[test]
    fn refresh_preserves_draft_and_conflict_until_explicit_acceptance() {
        let mut editor = Editor::default();
        editor.observe(ProjectRules {
            workflow: None,
            git: None,
            revision: 1,
            enabled: true,
            text: "Old".into(),
        });
        editor.draft.text = "Typing".into();
        editor.observe(ProjectRules {
            workflow: None,
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
