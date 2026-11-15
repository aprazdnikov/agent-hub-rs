//! Shared model-free administration used by the desktop and Telegram.
use super::memory_error;
use hub_memory::{
    AdminAction, AdminPreview, EntryDetails, ImportChoice, ImportPreview, MemoryExport,
    MemoryOverview, MemoryReader, SnapshotToken, StatusFilter, Store, VerifiedExport,
};
use std::{collections::BTreeMap, path::PathBuf};

pub const AUTO_WARNING: &str = "Старые ревизии будут удаляться без дополнительного подтверждения. Без экспорта восстановить их нельзя. Сохраняются последние 32 ревизии; при заполнении 8 МиБ удаляется старая история, включая архив. Последняя ревизия каждого ключа сохраняется.";
#[derive(Clone, Debug)]
pub enum Action {
    ReopenImport(ImportPreview),
    Compare {
        preview: ImportPreview,
        key: String,
        local_revision: Option<u32>,
        incoming_revision: Option<u32>,
    },
    Overview {
        filter: StatusFilter,
        query: String,
        offset: usize,
    },
    Details {
        key: String,
        revision: Option<u32>,
    },
    Preview(AdminAction),
    Apply(AdminPreview),
    Export(PathBuf),
    ImportFile(PathBuf),
    Import {
        export: MemoryExport,
        choices: BTreeMap<String, ImportChoice>,
    },
    ApplyImport(ImportPreview),
    PrepareReset(PathBuf),
    Reset {
        token: SnapshotToken,
        keep: Vec<String>,
        receipt: VerifiedExport,
    },
}
#[derive(Clone, Debug)]
pub enum Panel {
    Comparison {
        preview: ImportPreview,
        key: String,
        local_revision: Option<u32>,
        incoming_revision: Option<u32>,
    },
    Overview(MemoryOverview),
    Details(EntryDetails),
    Confirm(AdminPreview),
    Import {
        preview: ImportPreview,
        export: MemoryExport,
        choices: BTreeMap<String, ImportChoice>,
    },
    Reset {
        token: SnapshotToken,
        keep: Vec<String>,
        receipt: VerifiedExport,
    },
    Notice(String),
}
pub fn run(
    base: &std::path::Path,
    cwd: &std::path::Path,
    action: &Action,
) -> Result<Panel, String> {
    execute(base, cwd, action).map_err(memory_error)
}
fn execute(
    base: &std::path::Path,
    cwd: &std::path::Path,
    action: &Action,
) -> Result<Panel, hub_memory::Error> {
    let reader = MemoryReader::new(base, cwd)?;
    match action {
        Action::ReopenImport(preview) => execute(
            base,
            cwd,
            &Action::Import { export: preview.source_export(), choices: preview.choices() },
        ),
        Action::Compare { preview, key, local_revision, incoming_revision } => {
            preview.conflict(key)?;
            Ok(Panel::Comparison {
                preview: preview.clone(),
                key: key.clone(),
                local_revision: *local_revision,
                incoming_revision: *incoming_revision,
            })
        }
        Action::Overview { filter, query, offset } => {
            Ok(Panel::Overview(reader.overview(query, *offset, 20, *filter)?))
        }
        Action::Details { key, revision } => Ok(Panel::Details(reader.details(key, *revision)?)),
        Action::Export(path) => {
            reader.export()?.write_new(path)?;
            Ok(Panel::Notice(format!("Экспорт сохранён: {}", path.display())))
        }
        Action::ImportFile(path) => {
            let export = MemoryExport::decode(std::fs::File::open(path)?)?;
            execute(base, cwd, &Action::Import { export, choices: BTreeMap::new() })
        }
        Action::PrepareReset(path) => {
            let export = reader.export()?;
            let receipt = export.write_new(path)?;
            let token = reader.overview("", 0, 20, StatusFilter::All)?.token;
            let mut keep = Vec::new();
            let mut offset = 0;
            loop {
                let page = reader.overview("", offset, 20, StatusFilter::All)?.page;
                keep.extend(page.entries.into_iter().map(|e| e.key));
                offset += 20;
                if offset >= page.matched_count {
                    break;
                }
            }
            Ok(Panel::Reset { token, keep, receipt })
        }
        other => {
            let store = Store::open(base, cwd, "human-admin")?;
            match other {
                Action::Preview(action) => Ok(Panel::Confirm(store.preview(action.clone())?)),
                Action::Apply(preview) => {
                    store.apply(preview.clone())?;
                    Ok(Panel::Notice("Изменение сохранено. Обновите список.".into()))
                }
                Action::Import { export, choices } => Ok(Panel::Import {
                    preview: store.preview_import(export.clone(), choices.clone())?,
                    export: export.clone(),
                    choices: choices.clone(),
                }),
                Action::ApplyImport(preview) => {
                    store.apply_import(preview.clone())?;
                    Ok(Panel::Notice("Импорт завершён. Автоочистка не включалась.".into()))
                }
                Action::Reset { token, keep, receipt } => {
                    store.reset(token.clone(), keep.clone(), receipt.clone())?;
                    Ok(Panel::Notice("Новое поколение создано. Запустите новые агентские сессии; старые процессы писать не смогут.".into()))
                }
                _ => Err(hub_memory::Error::InvalidInput("expected administrative operation")),
            }
        }
    }
}
#[must_use]
pub fn preview_text(preview: &AdminPreview) -> String {
    let warning = match preview.action {
        AdminAction::SetPolicy(hub_memory::CleanupPolicy::Automatic) => AUTO_WARNING,
        AdminAction::Delete { .. } | AdminAction::Prune { .. } => {
            "Перед удалением рекомендуется экспорт. Без экспорта восстановить удалённые данные нельзя."
        }
        _ => "Подтвердите изменение выбранного проекта.",
    };
    format!(
        "{warning}\nДействие: {:?}\nУдаляемых ревизий: {} · ключей: {}\nРазмер: {} → {} байт",
        preview.action,
        preview.removed_revisions,
        preview.removed_keys.len(),
        preview.before_bytes,
        preview.after_bytes
    )
}

#[must_use]
pub fn status_name(status: hub_memory::MemoryStatus) -> &'static str {
    match status {
        hub_memory::MemoryStatus::Active => "активная",
        hub_memory::MemoryStatus::Stale => "устаревшая",
        hub_memory::MemoryStatus::Archived => "архивная",
    }
}
#[must_use]
pub fn policy_name(policy: hub_memory::CleanupPolicy) -> &'static str {
    match policy {
        hub_memory::CleanupPolicy::Manual => "ручная (рекомендуется)",
        hub_memory::CleanupPolicy::Automatic => "автоматическая",
    }
}
#[must_use]
pub fn import_name(kind: &hub_memory::ImportKind) -> String {
    match kind {
        hub_memory::ImportKind::New => "новая".into(),
        hub_memory::ImportKind::Identical => "без изменений".into(),
        hub_memory::ImportKind::Continued => "продолжение истории".into(),
        hub_memory::ImportKind::Conflict => "конфликт: оставить локальную".into(),
        hub_memory::ImportKind::Copied(key) => format!("копия → {key}"),
    }
}

#[must_use]
pub fn record_text(record: &hub_memory::ImportRecord<'_>, revision: Option<u32>) -> String {
    if let Some(deleted) = record.deleted_revision {
        return format!("Запись удалена; последняя ревизия r{deleted}.");
    }
    let selected = revision
        .and_then(|revision| record.history.iter().find(|entry| entry.revision == revision))
        .or_else(|| record.history.last());
    let Some(entry) = selected else {
        return "Запись отсутствует.".into();
    };
    format!(
        "Состояние: {} · история сокращена: {}\nДоступные ревизии: {:?}\n{}",
        record.status.map_or("отсутствует", status_name),
        record.history_pruned,
        record.history.iter().map(|entry| entry.revision).collect::<Vec<_>>(),
        super::entry_text(entry)
    )
}
