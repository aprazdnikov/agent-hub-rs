//! Консольный режим хаба: те же агенты без окна, трея и Telegram-канала.
//!
//! Режим нужен машине без дисплея — например Raspberry Pi: хаб запускается одной командой,
//! разговор идёт строками в терминале, а согласования агент получает там же. Настройки, ключи и
//! бэкенды те же, что и в окне (`settings.toml` и связка ключей), поэтому разговор, начатый в
//! консоли, продолжается в GUI на другой машине.
//!
//! Консольный режим не поднимает GUI-зависимости: ни `eframe`, ни tray, ни бота Telegram.

use std::collections::VecDeque;
use std::io::{self, BufRead, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use anyhow::{Context as _, anyhow};
use futures::future::BoxFuture;
use hub_agent::channel::UserChannel;
use hub_agent::conversation::{Conversation, Limits};
use hub_core::domain::{
    AbsolutePath, AgentEvent, BackendKind, Decision, Denied, FileDelivery, OutgoingFile, Prompt,
    Question, QuestionAnswer, QuestionOption, QuestionsOutcome, SessionId, ToolRequest, ToolUse,
    TopicSession,
};
use hub_core::options::{Effective, Mode, Preferences, Source};
use hub_core::settings::{ExecutionSettings, Settings};
use tokio::sync::{mpsc, watch};
use tokio_util::sync::CancellationToken;

use crate::config::{FileSettings, Loaded};
use crate::dirs::AppDirs;
use crate::gui::home_or_root;
use crate::instance::InstanceLock;
use crate::logging::{self, LogBuffer};
use crate::secrets::Keyring;

/// Ключ, по которому хаб открывается в терминале вместо окна.
pub const FLAGS: [&str; 2] = ["--cli", "--tui"];

const HELP: &str = concat!(
    "Команды:\n",
    "  /mode <значение> — режим бэкенда (для Pi — уровень размышления)\n",
    "  /model <провайдер/модель> — модель следующего хода\n",
    "  /status — модель, режим и рабочий каталог\n",
    "  /stop — прервать текущий ход (можно ввести во время работы)\n",
    "  /help — эта справка, /quit — выход\n",
    "Обычная строка — сообщение агенту.\n",
);

/// Нужно ли запускать консольный режим по первому аргументу.
#[must_use]
pub fn requested(first: Option<&str>) -> bool {
    first.is_some_and(|arg| FLAGS.contains(&arg))
}

/// Что попросили запустить; всё остальное хаб берёт из своих настроек.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Options {
    pub backend: Option<BackendKind>,
    pub dir: Option<PathBuf>,
    pub help: bool,
}

/// Разбор аргументов после ключа режима. Ошибка — текст для терминала.
pub fn parse(args: impl IntoIterator<Item = String>) -> Result<Options, String> {
    let mut options = Options::default();
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--backend" => {
                let value = args.next().ok_or("--backend требует имя бэкенда")?;
                options.backend = Some(
                    BackendKind::parse(&value)
                        .ok_or_else(|| format!("Неизвестный бэкенд «{value}»"))?,
                );
            }
            "--dir" => {
                let value = args.next().ok_or("--dir требует путь")?;
                options.dir = Some(PathBuf::from(value));
            }
            "--help" | "-h" => options.help = true,
            other if other.starts_with('-') => return Err(format!("Неизвестный ключ {other}")),
            other => return Err(format!("Лишний аргумент «{other}»")),
        }
    }
    Ok(options)
}

/// Запуск. Возвращается после `/quit`, конца ввода или ошибки настроек.
///
/// # Errors
///
/// Когда настройки не читаются или их ещё не заполняли: консольный режим, в отличие от окна,
/// не умеет просить токен бота и путь к ключам.
pub fn run(args: impl IntoIterator<Item = String>) -> anyhow::Result<()> {
    let options = parse(args).map_err(|error| anyhow!(error))?;
    if options.help {
        print!("{HELP}");
        return Ok(());
    }
    let dirs = AppDirs::locate().context("не найден домашний каталог пользователя")?;
    let lock = InstanceLock::acquire(&dirs.lock())
        .map_err(|_| anyhow!("agent-hub уже запущен: закройте окно или другой терминал"))?;
    let _guard = logging::init(&dirs.logs(), LogBuffer::default())?;
    let home = home_or_root();
    let store = FileSettings::new(dirs.settings(), home.clone(), Box::new(Keyring));
    let settings = match store.load()? {
        Loaded::Ready(settings) => settings,
        Loaded::Missing => {
            return Err(anyhow!(
                "настроек нет: запустите хаб с окном и заполните их, либо создайте settings.toml"
            ));
        }
        Loaded::Incomplete { errors, .. } => {
            let fields = errors
                .iter()
                .map(|error| format!("{:?}", error.field))
                .collect::<Vec<_>>()
                .join(", ");
            return Err(anyhow!(
                "настройки неполные: не хватает полей {fields}; допишите их в окне или в settings.toml"
            ));
        }
    };
    let workdir = match options.dir {
        Some(dir) => dir,
        None => std::env::current_dir().context("нет текущего каталога")?,
    };
    let Some(root) = AbsolutePath::new(workdir) else {
        return Err(anyhow!("рабочий каталог недоступен или не абсолютен"));
    };
    let backend = options.backend.unwrap_or(settings.default_backend);
    let program = std::env::current_exe().context("не найден путь самого себя")?;
    let runtime = tokio::runtime::Builder::new_multi_thread().enable_all().build()?;
    tracing::info!(
        version = env!("CARGO_PKG_VERSION"),
        backend = backend.name(),
        "console started"
    );
    // Блокировка держится весь разговор: второй экземпляр спорил бы с теми же настройками.
    runtime.block_on(session(settings, backend, root, program, &lock));
    Ok(())
}

/// Прогон цикла до конца ввода; блокирует текущий поток на всё время разговора.
async fn session(
    settings: Settings,
    backend: BackendKind,
    root: AbsolutePath,
    program: PathBuf,
    _lock: &InstanceLock,
) {
    let mut console = Console { settings, backend, root, program, input: Input::start() };
    console.repl().await;
}

/// Консоль одного запуска: настройки процесса, рабочий каталог и идущий разговор.
struct Console {
    settings: Settings,
    backend: BackendKind,
    root: AbsolutePath,
    program: PathBuf,
    input: Input,
}

impl Console {
    /// Цикл «строка пользователя → ход агента». Один запуск — один разговор в одном каталоге.
    async fn repl(&mut self) {
        let mut session: Option<SessionId> = None;
        let mut queued: Vec<Prompt> = Vec::new();
        println!(
            "agent-hub · бэкенд {} · каталог {} · /help для списка команд",
            self.backend.name(),
            self.root.as_path().display()
        );
        loop {
            print!("{} › ", self.backend.name());
            // Промпт виден сразу: без сброса буфера вывод через канал идёт вразнобой.
            if io::stdout().flush().is_err() {
                break;
            }
            let Some(line) = self.input.next().await else { break };
            match command(&line) {
                Command::Help => print!("{HELP}"),
                Command::Quit => break,
                Command::Status => {
                    println!("{}", status(&self.settings, self.backend, &self.root));
                }
                Command::Stop => println!("Агент сейчас не работает."),
                Command::Mode(raw) => say(set_mode(&mut self.settings, self.backend, raw)),
                Command::Model(raw) => say(set_model(&mut self.settings, self.backend, raw)),
                Command::Unknown(name) => {
                    println!("Неизвестная команда «{name}»; /help покажет список.");
                }
                Command::Turn(text) => match Prompt::new(text.to_owned(), Vec::new()) {
                    Some(prompt) => {
                        let outcome = self.turn(prompt, &mut session, &mut queued).await;
                        if let Err(error) = outcome {
                            println!("⚠ {error}");
                        }
                    }
                    None => println!("Пустое сообщение."),
                },
            }
        }
    }
}

/// Что делает строка, введённая у промпта.
#[derive(Debug, PartialEq, Eq)]
enum Command<'a> {
    Help,
    Quit,
    Status,
    Stop,
    Mode(&'a str),
    Model(&'a str),
    Unknown(&'a str),
    /// Обычное сообщение агенту.
    Turn(&'a str),
}

fn command(text: &str) -> Command<'_> {
    let text = text.trim();
    let Some(rest) = text.strip_prefix('/') else { return Command::Turn(text) };
    let (name, value) = match rest.split_once([' ', '\t']) {
        Some((name, value)) => (name, value.trim()),
        None => (rest, ""),
    };
    match name {
        "" => Command::Turn(text),
        "quit" | "exit" | "q" => Command::Quit,
        "help" | "h" | "?" => Command::Help,
        "status" => Command::Status,
        "stop" => Command::Stop,
        "mode" => Command::Mode(value),
        "model" => Command::Model(value),
        other => Command::Unknown(other),
    }
}

fn say(outcome: Result<String, String>) {
    match outcome {
        Ok(text) => println!("{text}"),
        Err(error) => println!("⚠ {error}"),
    }
}

/// С чем хаб сейчас работает: модель, режим и каталог — тем же `Effective`, что и в Telegram.
fn status(settings: &Settings, backend: BackendKind, root: &AbsolutePath) -> String {
    let effective = Preferences::default().resolve(backend, settings);
    match effective {
        Ok(effective) => format!("{}\nКаталог: {}", effective.describe(), root.as_path().display()),
        Err(error) => error,
    }
}

/// `/mode <значение>`: тот же разбор и та же проекция в настройки, что у темы в Telegram.
fn set_mode(settings: &mut Settings, backend: BackendKind, raw: &str) -> Result<String, String> {
    let available = Mode::all(backend);
    if raw.is_empty() {
        return if available.is_empty() {
            Ok(format!("У бэкенда {} режимов нет.", backend.name()))
        } else {
            let names: Vec<String> = available.iter().copied().map(Mode::wire).collect();
            Ok(format!(
                "Режимы: {} (текущий: {})",
                names.join(", "),
                current_mode(&available, settings, backend)
            ))
        };
    }
    let mode = Mode::parse(backend, raw)?.ok_or("Укажите режим после /mode")?;
    project(settings, backend, |effective| {
        effective.mode = Some(mode);
        effective.mode_source = Source::Topic;
    })?;
    Ok(format!("Режим: {}", mode.wire()))
}

/// `/model <провайдер/модель>`: проверка та же, что у поля модели в настройках.
fn set_model(settings: &mut Settings, backend: BackendKind, raw: &str) -> Result<String, String> {
    // `Options::parse` недоступен без черновика, поэтому проверяем те же признаки здесь.
    if raw.is_empty() {
        project(settings, backend, |effective| {
            effective.model = None;
            effective.model_source = Source::Global;
        })?;
        return Ok("Модель: выбор CLI/сохранённой сессии".to_owned());
    }
    if raw.len() > 256
        || raw.starts_with('-')
        || raw.chars().any(|ch| ch.is_whitespace() || ch.is_control())
    {
        return Err("ID модели: до 256 байт, без пробелов, управляющих символов и начального «-»"
            .to_owned());
    }
    project(settings, backend, |effective| {
        effective.model = Some(raw.to_owned());
        effective.model_source = Source::Topic;
    })?;
    Ok(format!("Модель: {raw}"))
}

fn current_mode(available: &[Mode], settings: &Settings, backend: BackendKind) -> String {
    Preferences::default()
        .resolve(backend, settings)
        .ok()
        .and_then(|effective| effective.mode)
        .filter(|mode| available.contains(mode))
        .map_or_else(|| "из конфигурации CLI/сессии".to_owned(), Mode::wire)
}

/// Одно изменение поверх текущих настроек: консоль пересобирает тот же `Effective`, что хаб
/// строит для темы, и правит в нём одно поле. Свежий `Effective` с пустыми полями стёр бы
/// вторую половину — режим сбрасывал модель, а модель — режим.
fn project(
    settings: &mut Settings,
    backend: BackendKind,
    change: impl FnOnce(&mut Effective),
) -> Result<(), String> {
    let mut effective = Preferences::default().resolve(backend, settings)?;
    change(&mut effective);
    effective.apply(settings);
    Ok(())
}

impl Console {
    /// Один ход: агент работает, события печатаются, строки в это время уточняют ход.
    async fn turn(
        &mut self,
        prompt: Prompt,
        session: &mut Option<SessionId>,
        queued: &mut Vec<Prompt>,
    ) -> anyhow::Result<()> {
        let topic =
            TopicSession::fresh(self.backend, self.root.clone()).with_session(session.clone());
        let (events, mut seen) = mpsc::channel::<AgentEvent>(64);
        let cancel = CancellationToken::new();
        let input = self.input.clone();
        let limits = Limits::new(self.settings.timeouts.background);
        let conversation = Conversation {
            channel: Arc::new(Terminal { input: input.clone(), cancel: cancel.clone() }),
            events,
            cancel: cancel.clone(),
            limits,
        };
        let (sender, inbox) = mpsc::channel::<Prompt>(8);
        for waiting in queued.drain(..) {
            // Что не успело уйти в прошлый ход, входит в этот как обычные сообщения.
            if sender.send(waiting).await.is_err() {
                break;
            }
        }
        let execution = Arc::new(ExecutionSettings::from(&self.settings));
        let program = self.program.clone();
        let mut running = Box::pin(hub_telegram::agents::execute_local(
            &topic,
            prompt,
            inbox,
            conversation,
            execution,
            &program,
        ));
        let mut text_open = false;
        let mut leftover: Option<mpsc::Receiver<Prompt>> = None;
        // Закрытый ввод — не команда остановиться: уточнений больше не будет, а ход
        // доигрывает до конца. Иначе select крутился бы на мгновенном `None`.
        let mut stdin_open = true;
        loop {
            tokio::select! {
                biased;
                done = &mut running, if leftover.is_none() => leftover = Some(done),
                event = seen.recv() => {
                    // Поток закрывается, когда ход завершён и события разобраны.
                    let Some(event) = event else { break };
                    print_event(&event, &mut text_open);
                    if let AgentEvent::Finished(finished) = &event {
                        *session = Some(finished.session.clone());
                    }
                }
                line = input.next(), if stdin_open => match line {
                    None => stdin_open = false,
                    Some(text) => steer(&sender, &text, &cancel).await,
                },
            }
        }
        if let Some(mut pending) = leftover {
            // Сообщения, которые агент не успел взять, переходят в следующий ход.
            while let Ok(waiting) = pending.try_recv() {
                queued.push(waiting);
            }
        }
        if text_open {
            println!();
        }
        if cancel.is_cancelled() {
            println!("· ход прерван");
        }
        Ok(())
    }
}

/// Строка, введённая во время хода: `/stop` прерывает, всё остальное — уточнение.
async fn steer(sender: &mpsc::Sender<Prompt>, text: &str, cancel: &CancellationToken) {
    match command(text) {
        Command::Stop | Command::Quit => cancel.cancel(),
        Command::Turn(body) => match Prompt::new(body.to_owned(), Vec::new()) {
            Some(prompt) => {
                if sender.send(prompt).await.is_err() {
                    println!("· сообщение не принято: ход уже закончился");
                }
            }
            None => println!("Пустое сообщение."),
        },
        Command::Mode(_) | Command::Model(_) => {
            println!("· режим и модель меняются между ходами: /stop, затем команда");
        }
        Command::Unknown(_) | Command::Help | Command::Status => {
            println!("· во время работы доступны только текст и /stop");
        }
    }
}

fn print_event(event: &AgentEvent, text_open: &mut bool) {
    match event {
        AgentEvent::TurnStarted => {}
        AgentEvent::AssistantText(text) => {
            print!("{text}");
            *text_open = true;
            io::stdout().flush().ok();
        }
        AgentEvent::ToolCall(tool) => println!("{}", tool_line(tool)),
        AgentEvent::Finished(finished) => {
            if *text_open {
                println!();
                *text_open = false;
            }
            println!("{}", hub_core::render::format_finished(finished));
        }
        AgentEvent::Failed(reason) => println!("⚠ {reason}"),
        AgentEvent::SessionStarted(id) => tracing::debug!(session = id.as_str(), "session started"),
        AgentEvent::BackgroundAbandoned(tasks) => {
            tracing::warn!(tasks = ?tasks, "background tasks abandoned");
        }
        AgentEvent::BackgroundChanged(count) => {
            tracing::debug!(count = *count, "background tasks changed");
        }
        AgentEvent::Quota(update) => tracing::debug!(?update, "quota reported"),
    }
}

fn tool_line(tool: &ToolUse) -> String {
    format!("· {} · {}", tool.tool, tool.summary)
}

/// Строки терминала: один источник и для команд, и для ответов агенту.
#[derive(Clone)]
struct Input(Arc<Shared>);

struct Shared {
    queue: Mutex<Queue>,
    /// Номер изменения очереди: ждущие сравнивают его со своим, поэтому уведомление
    /// не теряется, когда за строкой следят сразу REPL и диалог.
    changed: watch::Sender<u64>,
    closed: AtomicBool,
}

/// Строки ввода и счётчик открытых диалогов.
///
/// Диалог (согласование или вопрос агента) и цикл REPL тянут строки из одной очереди:
/// без счётчика REPL перехватывал ответ согласования, и агент получал отказ, хотя
/// оператор набрал «y».
struct Queue {
    lines: VecDeque<String>,
    dialogs: usize,
}

impl Default for Shared {
    fn default() -> Self {
        let (changed, _) = watch::channel(0);
        Self {
            queue: Mutex::new(Queue { lines: VecDeque::new(), dialogs: 0 }),
            changed,
            closed: AtomicBool::new(false),
        }
    }
}

impl Input {
    /// Читает stdin в отдельном потоке; канал закрывается вместе с вводом.
    fn start() -> Self {
        let shared = Arc::new(Shared::default());
        let weak = Arc::downgrade(&shared);
        std::thread::spawn(move || {
            let stdin = io::stdin();
            for line in stdin.lock().lines() {
                let Some(shared) = weak.upgrade() else { return };
                let Ok(text) = line else { break };
                deliver(&shared, text);
            }
            if let Some(shared) = weak.upgrade() {
                shared.closed.store(true, Ordering::Relaxed);
                shared.changed.send_modify(|generation| *generation += 1);
            }
        });
        Self(shared)
    }

    /// Строка для цикла REPL; `None`, когда ввод закрылся. Пока открыт диалог агента,
    /// очередь принадлежит ему, и здесь строки не берутся.
    async fn next(&self) -> Option<String> {
        self.take(true).await
    }

    /// Строка для самого диалога: он ждёт ответ даже когда открыт.
    async fn dialog_line(&self) -> Option<String> {
        self.take(false).await
    }

    async fn take(&self, for_repl: bool) -> Option<String> {
        // Подписка до проверки: строка, пришедшая между ними, видна либо в очереди,
        // либо в изменившемся номере.
        let mut seen = self.0.changed.subscribe();
        loop {
            if let Some(text) = lock(&self.0.queue).pop(for_repl) {
                return Some(text);
            }
            if self.0.closed.load(Ordering::Relaxed) {
                return None;
            }
            if seen.changed().await.is_err() {
                return None;
            }
        }
    }

    #[cfg(test)]
    fn dialogs(&self) -> usize {
        lock(&self.0.queue).dialogs
    }

    #[cfg(test)]
    fn detached() -> Self {
        Self(Arc::new(Shared::default()))
    }

    /// Открыть диалог: строки принадлежат ему, пока возвращённый страж жив.
    fn dialog(&self) -> Dialog {
        lock(&self.0.queue).dialogs += 1;
        Dialog(Arc::clone(&self.0))
    }
}

impl Queue {
    fn pop(&mut self, for_repl: bool) -> Option<String> {
        if for_repl && self.dialogs > 0 {
            return None;
        }
        self.lines.pop_front()
    }
}

/// Одна строка ввода: в очередь и разбудить тех, кто её ждёт.
fn deliver(shared: &Arc<Shared>, text: String) {
    lock(&shared.queue).lines.push_back(text);
    shared.changed.send_modify(|generation| *generation += 1);
}

/// Страж открытого диалога: снимает счётчик, когда агент получил ответ.
struct Dialog(Arc<Shared>);

impl Drop for Dialog {
    fn drop(&mut self) {
        let mut queue = lock(&self.0.queue);
        queue.dialogs = queue.dialogs.saturating_sub(1);
        drop(queue);
        // Цикл REPL спал, пока диалог ждал ответ: пора вернуться к очереди.
        self.0.changed.send_modify(|generation| *generation += 1);
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Пользователь прямо в терминале: согласования и вопросы агента приходят сюда же.
struct Terminal {
    input: Input,
    cancel: CancellationToken,
}

impl UserChannel for Terminal {
    fn request(&self, tool: ToolRequest) -> BoxFuture<'_, Decision> {
        let input = self.input.clone();
        let cancel = self.cancel.clone();
        Box::pin(async move {
            let dialog = input.dialog();
            println!("⚠ Агент просит разрешить {} · {}", tool.tool, tool.summary);
            print!("   разрешить? [y/N] ");
            io::stdout().flush().ok();
            let answer = tokio::select! {
                biased;
                () = cancel.cancelled() => None,
                answer = input.dialog_line() => answer,
            };
            drop(dialog);
            match answer.as_deref().map(str::trim) {
                Some("y" | "Y" | "yes" | "да") => Decision::Allowed,
                other => Decision::Denied(Denied::new(other.unwrap_or("нет ответа"))),
            }
        })
    }

    fn ask(&self, questions: Vec<Question>) -> BoxFuture<'_, QuestionsOutcome> {
        let input = self.input.clone();
        Box::pin(async move {
            let mut answers = Vec::with_capacity(questions.len());
            let dialog = input.dialog();
            for question in questions {
                println!("❓ {}", question.text());
                print_options(question.options());
                print!("   ответ: ");
                io::stdout().flush().ok();
                let Some(text) = input.dialog_line().await else {
                    drop(dialog);
                    return QuestionsOutcome::Denied(Denied::new("ввод закрылся"));
                };
                let text = text.trim();
                let answer = option_of(question.options(), text).unwrap_or(text);
                answers.push(QuestionAnswer {
                    question: question.text().to_owned(),
                    answer: answer.to_owned(),
                });
            }
            drop(dialog);
            QuestionsOutcome::Answered(answers)
        })
    }

    /// В консоли нет чата, куда файл можно приложить: путь остаётся виден оператору.
    fn send_file(&self, file: OutgoingFile) -> BoxFuture<'_, FileDelivery> {
        Box::pin(async move {
            let caption = if file.caption.is_empty() {
                String::new()
            } else {
                format!(" — {}", file.caption)
            };
            println!("📎 Файл готов: {}{caption}", file.path);
            FileDelivery::Delivered
        })
    }
}

fn print_options(options: &[QuestionOption]) {
    for (number, option) in options.iter().enumerate() {
        println!("   {}. {} — {}", number + 1, option.label, option.description);
    }
}

/// Ответ по номеру варианта, если он назван числом.
fn option_of<'a>(options: &'a [QuestionOption], text: &str) -> Option<&'a str> {
    let number: usize = text.parse().ok()?;
    options.get(number.checked_sub(1)?).map(|option| option.label.as_str())
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use hub_core::domain::Selection;
    use rstest::rstest;

    use super::*;
    use hub_core::settings::{Draft, PiThinking};

    #[rstest]
    #[case(["--backend".into(), "pi".into()].into_iter(), Options { backend: Some(BackendKind::Pi), dir: None, help: false })]
    #[case(["--dir".into(), "/tmp".into()].into_iter(), Options { backend: None, dir: Some(PathBuf::from("/tmp")), help: false })]
    #[case([].into_iter(), Options::default())]
    #[case(["--help".into()].into_iter(), Options { backend: None, dir: None, help: true })]
    fn arguments_are_parsed(#[case] args: impl Iterator<Item = String>, #[case] expected: Options) {
        assert_eq!(parse(args), Ok(expected));
    }

    #[rstest]
    #[case("--backend")]
    #[case("--backend telegram")]
    #[case("--dir")]
    #[case("--window")]
    #[case("случайное сообщение")]
    fn bad_arguments_are_named(#[case] raw: &str) {
        assert!(parse([raw.to_owned()].into_iter()).is_err());
    }

    #[rstest]
    #[case("--cli", true)]
    #[case("--tui", true)]
    #[case("--memory-mcp", false)]
    #[case("", false)]
    fn flags_start_the_console(#[case] first: &str, #[case] expected: bool) {
        assert_eq!(requested(Some(first)), expected);
        assert!(!requested(None));
    }

    #[rstest]
    #[case("работай", Command::Turn("работай"))]
    #[case("  work  ", Command::Turn("work"))]
    #[case("/quit", Command::Quit)]
    #[case("/q", Command::Quit)]
    #[case("/help", Command::Help)]
    #[case("/status", Command::Status)]
    #[case("/stop", Command::Stop)]
    #[case("/mode high", Command::Mode("high"))]
    #[case("/mode   high ", Command::Mode("high"))]
    #[case("/mode", Command::Mode(""))]
    #[case("/model openai-codex/gpt-5.6-luna", Command::Model("openai-codex/gpt-5.6-luna"))]
    #[case("/unknown", Command::Unknown("unknown"))]
    #[case("/ текст", Command::Turn("/ текст"))]
    fn lines_become_commands<'a>(#[case] raw: &str, #[case] expected: Command<'a>) {
        assert_eq!(command(raw), expected);
    }

    #[rstest]
    #[case(PiThinking::Off)]
    #[case(PiThinking::Minimal)]
    #[case(PiThinking::Low)]
    #[case(PiThinking::Medium)]
    #[case(PiThinking::High)]
    #[case(PiThinking::Xhigh)]
    fn mode_reaches_the_backend_settings(#[case] thinking: PiThinking) {
        let mut settings = settings();
        let raw = Mode::Pi(thinking).wire();
        set_mode(&mut settings, BackendKind::Pi, &raw).unwrap();
        assert_eq!(settings.pi.thinking, thinking);
    }

    /// Регрессия живого прогона: оператор набирает «y», а цикл REPL перехватывает строку
    /// как уточнение хода, и агент получает отказ вместо разрешения.
    #[tokio::test]
    async fn an_open_dialog_takes_the_line_ahead_of_the_repl() {
        let input = Input::detached();
        let dialog = input.dialog();
        deliver(&input.0, "y".to_owned());
        let stolen = tokio::time::timeout(Duration::from_millis(50), input.next()).await;
        assert!(stolen.is_err(), "REPL взял строку у открытого диалога");
        assert_eq!(input.dialog_line().await.as_deref(), Some("y"));
        drop(dialog);
        deliver(&input.0, "следующее".to_owned());
        assert_eq!(input.next().await.as_deref(), Some("следующее"));
    }

    /// Тот же счётчик держит и вопросы агента: ответ уходит агенту, а не в очередь хода.
    #[tokio::test]
    async fn a_question_takes_the_line_ahead_of_the_repl() {
        let input = Input::detached();
        // Цикл REPL сидит на очереди всё это время — как во время настоящего хода.
        let reader = input.clone();
        let watching = tokio::spawn(async move {
            let mut taken = Vec::new();
            while let Some(line) = reader.next().await {
                taken.push(line);
            }
            taken
        });
        let terminal = Terminal { input: input.clone(), cancel: CancellationToken::new() };
        let asking = tokio::spawn(async move {
            let question = Question::new(
                "Куда кладём?".to_owned(),
                "Документация".to_owned(),
                Vec::new(),
                Selection::Single,
            )
            .unwrap();
            terminal.ask(vec![question]).await
        });
        while input.dialogs() == 0 {
            tokio::task::yield_now().await;
        }
        deliver(&input.0, "в docs".to_owned());
        let outcome = tokio::time::timeout(Duration::from_secs(5), asking).await.unwrap().unwrap();
        let QuestionsOutcome::Answered(answers) = outcome else {
            panic!("вопрос не был отвечен: {outcome:?}");
        };
        assert_eq!(answers[0].answer, "в docs");
        // Ввод закрывается: так видно, что REPL за время вопроса не утащил ни строки.
        input.0.closed.store(true, Ordering::Relaxed);
        input.0.changed.send_modify(|generation| *generation += 1);
        assert_eq!(watching.await.unwrap(), Vec::<String>::new());
    }

    /// Регрессия живой проверки: `/mode` после `/model` сбрасывал модель, потому что
    /// команда строила свежий `Effective` вместо текущего.
    #[test]
    fn model_and_mode_survive_each_other() {
        let mut settings = settings();
        set_model(&mut settings, BackendKind::Pi, "qwen-token-plan/qwen4-max").unwrap();
        set_mode(&mut settings, BackendKind::Pi, "high").unwrap();
        set_model(&mut settings, BackendKind::Pi, "openai-codex/gpt-5.6-luna").unwrap();
        assert_eq!(settings.pi.model.as_deref(), Some("openai-codex/gpt-5.6-luna"));
        assert_eq!(settings.pi.thinking, PiThinking::High);
    }

    #[test]
    fn empty_model_returns_the_choice_to_the_cli() {
        let mut settings = settings();
        set_model(&mut settings, BackendKind::Pi, "qwen-token-plan/qwen4-max").unwrap();
        set_mode(&mut settings, BackendKind::Pi, "high").unwrap();
        set_model(&mut settings, BackendKind::Pi, "").unwrap();
        // Модель снова упирается в выбор самого CLI, а режим остаётся.
        assert_eq!(settings.pi.model, None);
        assert_eq!(settings.pi.thinking, PiThinking::High);
        assert!(
            status(&settings, BackendKind::Pi, &root()).starts_with(
                "Модель: выбор CLI/сохранённой сессии (не подтверждён) · общие настройки"
            ),
            "{}",
            status(&settings, BackendKind::Pi, &root())
        );
    }

    fn root() -> AbsolutePath {
        AbsolutePath::new(std::env::temp_dir()).unwrap()
    }

    #[test]
    fn unknown_mode_is_refused_without_touching_settings() {
        let mut settings = settings();
        let before = settings.pi.thinking;
        assert!(set_mode(&mut settings, BackendKind::Pi, "ultra").is_err());
        assert_eq!(settings.pi.thinking, before);
    }

    #[test]
    fn empty_mode_lists_choices_without_an_error() {
        let mut settings = settings();
        let listed = set_mode(&mut settings, BackendKind::Pi, "").unwrap();
        assert!(listed.contains("high"));
        assert!(
            set_mode(&mut settings, BackendKind::Opencode, "").unwrap().contains("режимов нет")
        );
    }

    #[rstest]
    #[case("qwen-token-plan/qwen4-max", true)]
    #[case("openai-codex/gpt-5.6-luna", true)]
    #[case("", true)]
    #[case("-model", false)]
    #[case("two words", false)]
    #[case("\u{7}", false)]
    fn model_names_follow_the_settings_rules(#[case] raw: &str, #[case] accepted: bool) {
        let mut settings = settings();
        assert_eq!(set_model(&mut settings, BackendKind::Pi, raw).is_ok(), accepted);
        if accepted && !raw.is_empty() {
            assert_eq!(settings.pi.model.as_deref(), Some(raw));
        }
        if raw.is_empty() {
            assert_eq!(settings.pi.model, None);
        }
    }

    #[rstest]
    #[case("2", "высокий")]
    #[case("1", "низкий")]
    #[case("низкий", "низкий")]
    #[case("9", "9")]
    #[case("", "")]
    fn answers_accept_option_numbers(#[case] text: &str, #[case] expected: &str) {
        let options = options();
        assert_eq!(option_of(&options, text).unwrap_or(text), expected);
    }

    fn options() -> Vec<QuestionOption> {
        vec![
            QuestionOption {
                label: "низкий".to_owned(), description: "быстро".to_owned()
            },
            QuestionOption {
                label: "высокий".to_owned(), description: "качественно".to_owned()
            },
        ]
    }

    fn settings() -> Settings {
        let root = std::env::temp_dir();
        let draft = Draft {
            token: "1:token".to_owned(),
            chat: "-100".to_owned(),
            users: "1".to_owned(),
            workspace_root: root.display().to_string(),
            default_backend: BackendKind::Pi,
            ..Draft::default()
        };
        draft.parse(&root).unwrap()
    }
}
