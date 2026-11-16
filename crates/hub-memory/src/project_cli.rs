//! Human-only project binding administration, separate from the MCP tool surface.
use crate::{BindingMode, BindingPreview, Error};
use std::{ffi::OsString, path::PathBuf};

pub fn run_project_cli(args: Vec<OsString>) -> Result<String, String> {
    let options = Options::parse(args)?;
    let preview =
        BindingPreview::prepare(&options.base, &options.source, &options.target, options.mode)
            .map_err(Error::user_message)?;
    if let Some(confirmation) = options.confirmation {
        if confirmation != preview.confirmation() {
            return Err("Предварительный просмотр изменился или токен неверен. Повторите команду без --confirm.".into());
        }
        preview.apply().map_err(Error::user_message)?;
        return Ok(
            "Привязка памяти сохранена. Откройте новую агентскую сессию в целевом каталоге.".into(),
        );
    }
    Ok(format!(
        "Источник: {}\nНазначение: {}\nID памяти: {}\nРежим: {}\nОбщими будут все записи, история и политика очистки. Чаты и настройки сессий не переносятся.\nДля подтверждения повторите команду с --confirm {}",
        preview.source().display(),
        preview.target().display(),
        preview.project_id(),
        match preview.mode() {
            BindingMode::Move => "перенос (прежний путь будет отключён)",
            BindingMode::Share => "совместная память (оба пути остаются подключены)",
        },
        preview.confirmation()
    ))
}
struct Options {
    base: PathBuf,
    source: PathBuf,
    target: PathBuf,
    mode: BindingMode,
    confirmation: Option<OsString>,
}
impl Options {
    fn parse(args: Vec<OsString>) -> Result<Self, String> {
        let mut args = args.into_iter();
        let mode = match args.next().as_deref().and_then(std::ffi::OsStr::to_str) {
            Some("move") => BindingMode::Move,
            Some("share") => BindingMode::Share,
            _ => return Err("Использование: --memory-project move|share --from PATH --to PATH [--store-dir PATH] [--confirm TOKEN]".into()),
        };
        let (mut source, mut target, mut base, mut confirmation) = (None, None, None, None);
        while let Some(option) = args.next() {
            let slot = match option.to_str() {
                Some("--from") => &mut source,
                Some("--to") => &mut target,
                Some("--store-dir") => &mut base,
                Some("--confirm") => &mut confirmation,
                _ => return Err("Неизвестный аргумент привязки памяти".into()),
            };
            if slot.is_some() {
                return Err("Повтор аргумента привязки памяти".into());
            }
            *slot = Some(args.next().ok_or("Нет значения аргумента")?);
        }
        let source = PathBuf::from(source.ok_or("Укажите --from")?);
        let target = PathBuf::from(target.ok_or("Укажите --to")?);
        let base = base.map_or_else(
            || {
                directories::BaseDirs::new()
                    .map(|dirs| dirs.home_dir().join(".agent-hub/memory"))
                    .ok_or("Не найден домашний каталог")
            },
            |path| Ok(PathBuf::from(path)),
        )?;
        if [&source, &target, &base].iter().any(|path| !path.is_absolute()) {
            return Err("Пути должны быть абсолютными".into());
        }
        Ok(Self { base, source, target, mode, confirmation })
    }
}
