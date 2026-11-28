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
        if self.rules.saved.git.is_none() && task.git.is_none() && !task.isolated {
            return;
        }
        ui.strong("Подготовка Git");
        if task.isolated {
            ui.monospace(format!("hub/task-{}   /   {}", task.id, task.execution_dir().display()));
        }
        if let Some(git) = &task.git {
            ui.add(egui::Label::new(git.describe()).selectable(true));
        }
        if let Some(policy) = &self.rules.saved.git {
            ui.label(format!("База: {}/{}", policy.remote, policy.base));
            if task.runs.is_empty() {
                let editing = !(task.queue.is_some()
                    || task.archived
                    || matches!(task.status, TaskStatus::Accepted | TaskStatus::Deferred)
                    || self.git_busy
                    || self.mutation_pending()
                    || self.local_context() && self.local_block != super::LocalBlock::Free);
                let branch = self.git_branches.entry(task.id).or_insert_with(|| {
                    task.git
                        .as_ref()
                        .map_or_else(|| format!("task/{}", task.id), |git| git.branch.clone())
                });
                ui.add_enabled(
                    editing,
                    kylig_ui::TextField::new(branch)
                        .label("Новая рабочая ветка")
                        .width(ui.available_width())
                        .id_salt(("task_git_branch", &task.project, task.id))
                        .max_chars(160),
                );
                let branch = branch.clone();
                let valid = hub_core::tasks::git::valid_branch(&branch) && branch != policy.base;
                if !valid {
                    ui.weak("Введите допустимое имя рабочей ветки, отличное от базовой.");
                }
                let allowed = valid
                    && editing
                    && !self.rules.dirty()
                    && !task.git.as_ref().is_some_and(|git| {
                        git.state == PreparationState::Ready
                            && git.policy == *policy
                            && git.branch == branch
                    });
                if ui.add_enabled(allowed, kylig_ui::Button::new("Подготовить ветку Git")).clicked()
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
                    kylig_ui::Button::new("Остановить подготовку / проверку Git"),
                )
                .clicked()
        {
            self.request(Request::CancelGit, send);
        }
        ui.small("Подготовка не запускает агента. При ошибке проверьте Git: часть действий могла выполниться.");
    }
}
