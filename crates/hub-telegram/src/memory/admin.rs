use super::{Binding, Browser, Button, Request, entry_text, lifecycle};
use hub_memory::{
    AdminAction, CleanupPolicy, ImportChoice, ImportKind, MemoryStatus, StatusFilter,
};
use lifecycle::{Action, Panel};
use std::fmt::Write;

pub(super) fn admin(action: Action) -> Request {
    Request::Admin(Box::new(action))
}
pub(super) fn parse_admin(args: &str) -> Result<Request, String> {
    let parts: Vec<_> = args.split_whitespace().collect();
    let action=match parts.as_slice() {
        ["status"]=>Action::Overview{filter:StatusFilter::All,query:String::new(),offset:0},
        ["filter",filter,rest @ ..]=>Action::Overview{filter:match *filter {"active"=>StatusFilter::Active,"stale"=>StatusFilter::Stale,"archived"=>StatusFilter::Archived,"all"=>StatusFilter::All,_=>return Err("Фильтр: active/stale/archived/all".into())},query:rest.join(" "),offset:0},
        ["policy","auto"]=>Action::Preview(AdminAction::SetPolicy(CleanupPolicy::Automatic)),
        ["policy","manual"]=>Action::Preview(AdminAction::SetPolicy(CleanupPolicy::Manual)),
        [status @ ("archive"|"stale"|"activate"),key]=>Action::Preview(AdminAction::SetStatus{key:(*key).into(),status:match *status {"archive"=>MemoryStatus::Archived,"stale"=>MemoryStatus::Stale,_=>MemoryStatus::Active}}),
        ["delete",keys @ ..] if !keys.is_empty()=>Action::Preview(AdminAction::Delete{keys:keys.iter().map(|s|(*s).into()).collect()}),
        ["prune",key,numbers @ ..] if !numbers.is_empty()=>Action::Preview(AdminAction::Prune{revisions:std::collections::BTreeMap::from([((*key).into(),numbers.iter().map(|s|s.parse::<u32>()).collect::<Result<Vec<_>,_>>().map_err(|_|"Укажите номера ревизий через пробел")?)])}),
        _=>return Err("/memory · search ТЕКСТ · get КЛЮЧ [РЕВИЗИЯ] · status · filter active|stale|archived|all · policy auto|manual · archive|stale|activate КЛЮЧ · prune КЛЮЧ РЕВИЗИИ · delete КЛЮЧИ · export · import · reset · context · procedure · cancel".into()),
    };
    Ok(admin(action))
}
impl Browser {
    pub(super) fn admin_card(
        &mut self,
        panel: &Panel,
        binding: &Binding,
        keyboard: &mut Vec<Vec<Button>>,
    ) -> String {
        match panel {
            Panel::Context(packet) => {
                keyboard.push(vec![self.button(
                    "Проверить и получить текст пакета".into(),
                    Binding {
                        request: admin(Action::ValidateContext(packet.clone())),
                        ..binding.clone()
                    },
                )]);
                super::context::summary(packet)
            }
            Panel::ContextReady(packet) => packet.text().into(),

            Panel::Binding(preview) => {
                keyboard.push(vec![self.button(
                    "Подтвердить привязку".into(),
                    Binding {
                        request: admin(Action::ApplyBinding(preview.clone())),
                        ..binding.clone()
                    },
                )]);
                lifecycle::binding_text(preview)
            }

            Panel::Comparison { preview, key, local_revision, incoming_revision } => self
                .comparison_card(
                    preview,
                    key,
                    *local_revision,
                    *incoming_revision,
                    binding,
                    keyboard,
                ),
            Panel::Notice(text) => text.clone(),
            Panel::Overview(overview) => self.overview_card(overview, binding, keyboard),
            Panel::Details(details) => self.details_card(details, binding, keyboard),
            Panel::Confirm(preview) => {
                let label =
                    if matches!(preview.action, AdminAction::SetPolicy(CleanupPolicy::Automatic)) {
                        "Включить автоочистку"
                    } else {
                        "Подтвердить изменение"
                    };
                keyboard.push(vec![self.button(
                    label.into(),
                    Binding { request: admin(Action::Apply(preview.clone())), ..binding.clone() },
                )]);
                format!(
                    "Проект: {}\n{}\nЭкспорт перед удалением: /memory export",
                    binding.cwd.display(),
                    lifecycle::preview_text(preview)
                )
            }
            Panel::Import { preview, export, choices } => {
                self.import_card(preview, export, choices, binding, keyboard)
            }
            Panel::Reset { token, keep, receipt } => {
                for key in keep.iter().take(20) {
                    let selected = keep.iter().filter(|k| *k != key).cloned().collect();
                    keyboard.push(vec![self.button(
                        format!("Не сохранять {key}"),
                        Binding {
                            request: Request::ResetSelection {
                                token: token.clone(),
                                keep: selected,
                                receipt: receipt.clone(),
                            },
                            ..binding.clone()
                        },
                    )]);
                }
                keyboard.push(vec![self.button(
                    "Создать новое поколение".into(),
                    Binding {
                        request: admin(Action::Reset {
                            token: token.clone(),
                            keep: keep.clone(),
                            receipt: receipt.clone(),
                        }),
                        ..binding.clone()
                    },
                )]);
                format!(
                    "Экспорт проверен и отправлен. Сохранить ключи: {}. Остальные записи и отметки удаления будут удалены. Подтвердите новое поколение для {}. После этого нужны новые агентские сессии. Полный выбор: /memory reset-keep КЛЮЧИ (без ключей — ничего не сохранять).",
                    keep.join(", "),
                    binding.cwd.display()
                )
            }
        }
    }
}

impl Browser {
    fn overview_card(
        &mut self,
        overview: &hub_memory::MemoryOverview,
        binding: &Binding,
        keyboard: &mut Vec<Vec<Button>>,
    ) -> String {
        for entry in &overview.page.entries {
            keyboard.push(vec![self.button(
                format!("{} · r{}", entry.key, entry.revision),
                Binding {
                    request: admin(Action::Details { key: entry.key.clone(), revision: None }),
                    ..binding.clone()
                },
            )]);
        }
        if let Request::Admin(action) = &binding.request
            && let Action::Overview { filter, query, offset } = action.as_ref()
        {
            for next in [
                offset.checked_sub(20),
                (offset + 20 < overview.page.matched_count).then_some(offset + 20),
            ]
            .into_iter()
            .flatten()
            {
                keyboard.push(vec![self.button(
                    format!("Страница {}", next / 20 + 1),
                    Binding {
                        request: admin(Action::Overview {
                            filter: *filter,
                            query: query.clone(),
                            offset: next,
                        }),
                        ..binding.clone()
                    },
                )]);
            }
        }
        for (label, policy) in [
            ("Ручная очистка", CleanupPolicy::Manual),
            ("Настроить автоочистку", CleanupPolicy::Automatic),
        ] {
            keyboard.push(vec![self.button(
                label.into(),
                Binding {
                    request: admin(Action::Preview(AdminAction::SetPolicy(policy))),
                    ..binding.clone()
                },
            )]);
        }
        format!(
            "Проект: {}\n{}\nОчистка: {}\nКлючи: {}/500 · отметки удаления: {}/4096 · {} / 8388608 байт\nПоследняя очистка: {:?}\nФильтр: /memory filter all|active|stale|archived\nПеренос: /memory export · /memory import\nНовое поколение: /memory reset",
            overview.page.project.display(),
            lifecycle::binding_scope(&overview.page.bound_projects),
            lifecycle::policy_name(overview.policy),
            overview.page.key_count,
            overview.tombstone_count,
            overview.page.bytes,
            overview.last_cleanup
        )
    }
    fn details_card(
        &mut self,
        details: &hub_memory::EntryDetails,
        binding: &Binding,
        keyboard: &mut Vec<Vec<Button>>,
    ) -> String {
        for revision in &details.revisions {
            if *revision != details.entry.revision {
                keyboard.push(vec![self.button(
                    format!("r{revision}"),
                    Binding {
                        request: admin(Action::Details {
                            key: details.entry.key.clone(),
                            revision: Some(*revision),
                        }),
                        ..binding.clone()
                    },
                )]);
            }
        }
        for (label, status) in [
            ("В архив", MemoryStatus::Archived),
            ("Устарела", MemoryStatus::Stale),
            ("Активна", MemoryStatus::Active),
        ] {
            keyboard.push(vec![self.button(
                label.into(),
                Binding {
                    request: admin(Action::Preview(AdminAction::SetStatus {
                        key: details.entry.key.clone(),
                        status,
                    })),
                    ..binding.clone()
                },
            )]);
        }
        keyboard.push(vec![self.button(
            "Удалить запись…".into(),
            Binding {
                request: admin(Action::Preview(AdminAction::Delete {
                    keys: vec![details.entry.key.clone()],
                })),
                ..binding.clone()
            },
        )]);
        format!(
            "Состояние: {} · история сокращена: {}\n{}\nОчистка отдельных ревизий: /memory prune {} НОМЕРА",
            lifecycle::status_name(details.status),
            details.history_pruned,
            entry_text(&details.entry),
            details.entry.key
        )
    }
}

impl Browser {
    pub(super) fn edit_draft(
        &mut self,
        key: super::TopicKey,
        user: super::UserId,
        cwd: &std::path::Path,
        args: &str,
    ) -> bool {
        let words: Vec<_> = args.split_whitespace().collect();
        if !matches!(words.first(), Some(&"copy" | &"reset-keep" | &"compare")) {
            return false;
        }
        let Some((binding, panel, _)) = self.admin_draft.as_ref().filter(|(binding, _, time)| {
            binding.key == key
                && binding.user == user
                && binding.cwd == cwd
                && time.elapsed() < std::time::Duration::from_mins(10)
        }) else {
            self.say(key,"Нет актуального предварительного просмотра импорта/пересоздания для этого проекта.".into());
            return true;
        };
        let request = match (words.as_slice(), panel) {
            (["compare", key], Panel::Import { preview, .. }) => admin(Action::Compare {
                preview: preview.clone(),
                key: (*key).into(),
                local_revision: None,
                incoming_revision: None,
            }),
            (["copy", source, target], Panel::Import { export, choices, .. }) => {
                let mut choices = choices.clone();
                choices.insert((*source).into(), ImportChoice::CopyAs((*target).into()));
                admin(Action::Import { export: export.clone(), choices })
            }
            (["reset-keep", keys @ ..], Panel::Reset { token, receipt, .. }) => {
                Request::ResetSelection {
                    token: token.clone(),
                    keep: keys.iter().map(|key| (*key).to_owned()).collect(),
                    receipt: receipt.clone(),
                }
            }
            _ => {
                self.say(key,"/memory copy ИСХОДНЫЙ_КЛЮЧ НОВЫЙ_КЛЮЧ или /memory reset-keep СОХРАНЯЕМЫЕ_КЛЮЧИ".into());
                return true;
            }
        };
        self.start(Binding { request, ..binding.clone() });
        true
    }
}

impl Browser {
    fn comparison_card(
        &mut self,
        preview: &hub_memory::ImportPreview,
        key: &str,
        local_revision: Option<u32>,
        incoming_revision: Option<u32>,
        binding: &Binding,
        keyboard: &mut Vec<Vec<Button>>,
    ) -> String {
        let Ok(conflict) = preview.conflict(key) else {
            return "Нет записи в предпросмотре.".into();
        };
        let mut text = String::new();
        for (label, record, selected, local) in [
            ("Локальная запись", &conflict.local, local_revision, true),
            ("Входящая запись", &conflict.incoming, incoming_revision, false),
        ] {
            let _ = write!(text, "{label}\n{}\n\n", lifecycle::record_text(record, selected));
            for entry in record.history {
                keyboard.push(vec![self.button(
                    format!("{label}: r{}", entry.revision),
                    Binding {
                        request: admin(Action::Compare {
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
                        }),
                        ..binding.clone()
                    },
                )]);
            }
        }
        keyboard.push(vec![self.button(
            "Вернуться к решениям импорта".into(),
            Binding { request: admin(Action::ReopenImport(preview.clone())), ..binding.clone() },
        )]);
        text
    }
}

impl Browser {
    fn import_card(
        &mut self,
        preview: &hub_memory::ImportPreview,
        export: &hub_memory::MemoryExport,
        choices: &std::collections::BTreeMap<String, ImportChoice>,
        binding: &Binding,
        keyboard: &mut Vec<Vec<Button>>,
    ) -> String {
        for item in preview.items.iter().filter(|item| item.kind == ImportKind::Conflict).take(10) {
            if item.kind == ImportKind::Conflict {
                keyboard.push(vec![self.button(
                    format!("Сравнить {}", item.key),
                    Binding {
                        request: admin(Action::Compare {
                            preview: preview.clone(),
                            key: item.key.clone(),
                            local_revision: None,
                            incoming_revision: None,
                        }),
                        ..binding.clone()
                    },
                )]);
                let mut choices = choices.clone();
                let name = format!("{}-import", item.key.chars().take(57).collect::<String>());
                choices.insert(item.key.clone(), ImportChoice::CopyAs(name.clone()));
                keyboard.push(vec![self.button(
                    format!("Копировать {} → {name}", item.key),
                    Binding {
                        request: admin(Action::Import { export: export.clone(), choices }),
                        ..binding.clone()
                    },
                )]);
            }
        }
        keyboard.push(vec![self.button(
            "Подтвердить импорт в этот проект".into(),
            Binding { request: admin(Action::ApplyImport(preview.clone())), ..binding.clone() },
        )]);
        format!(
            "Импорт из {}\nВ проект: {}\nРазмер после импорта: {}\n{}\nКонфликты по умолчанию пропускаются. Можно выбрать копию; своё имя: /memory copy ИСХОДНЫЙ_КЛЮЧ НОВЫЙ_КЛЮЧ. Автоочистка не включается.",
            preview.source.display(),
            preview.target.display(),
            preview.after_bytes,
            preview
                .items
                .iter()
                .map(|item| format!("{}: {}", item.key, lifecycle::import_name(&item.kind)))
                .collect::<Vec<_>>()
                .join("\n")
        )
    }
}
