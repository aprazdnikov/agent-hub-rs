use super::{Command, Request, Task, TasksView};
use eframe::egui;
use hub_core::tasks::{TaskStatus, git::PreparationState};
impl TasksView {
    pub(super) fn git_controls(
        &mut self,
        ui: &mut egui::Ui,
        task: &Task,
        send: &mut impl FnMut(Command),
    ) {
        if self.rules.saved.git.is_none() && task.git.is_none() {
            return;
        }
        ui.strong("Подготовка Git");
        if let Some(git) = &task.git {
            ui.add(egui::Label::new(git.describe()).selectable(true));
        }
        if let Some(policy) = &self.rules.saved.git {
            ui.label(format!("База: {}/{}", policy.remote, policy.base));
            if task.runs.is_empty() {
                let editing = !(self.git_busy
                    || self.mutation_pending()
                    || self.local_context() && self.local_block != super::LocalBlock::Free);
                let branch = self.git_branches.entry(task.id).or_insert_with(|| {
                    task.git
                        .as_ref()
                        .map_or_else(|| format!("task/{}", task.id), |git| git.branch.clone())
                });
                ui.label("Новая рабочая ветка");
                ui.add_enabled(
                    editing,
                    egui::TextEdit::singleline(branch)
                        .id_salt(("task_git_branch", &task.project, task.id))
                        .char_limit(160),
                );
                let branch = branch.clone();
                let allowed = editing
                    && !self.git_busy
                    && !self.mutation_pending()
                    && !self.rules.dirty()
                    && !task.archived
                    && !matches!(task.status, TaskStatus::Accepted | TaskStatus::Deferred)
                    && !task.git.as_ref().is_some_and(|git| {
                        git.state == PreparationState::Ready
                            && git.policy == *policy
                            && git.branch == branch
                    });
                if ui.add_enabled(allowed, egui::Button::new("Подготовить ветку Git")).clicked()
                {
                    self.request(
                        Request::PrepareGitChecked {
                            id: task.id,
                            revision: task.revision,
                            branch,
                            rules_revision: self.rules.saved.revision,
                        },
                        send,
                    );
                }
            }
        }
        if self.git_busy
            && !self.local_context()
            && ui
                .add_enabled(
                    !self.mutation_pending(),
                    egui::Button::new("Остановить подготовку / проверку Git"),
                )
                .clicked()
        {
            self.request(Request::CancelGit, send);
        }
        ui.small("Подготовка не запускает агента. При ошибке проверьте Git: часть действий могла выполниться.");
        ui.separator();
    }
}
