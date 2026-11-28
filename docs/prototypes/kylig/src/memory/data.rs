use std::collections::BTreeMap;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Status {
    Active,
    Stale,
    Archived,
}

impl Status {
    pub const ALL: [Self; 3] = [Self::Active, Self::Stale, Self::Archived];
    pub fn label(self) -> &'static str {
        match self {
            Self::Active => "Активная",
            Self::Stale => "Устаревшая",
            Self::Archived => "В архиве",
        }
    }
}

#[derive(Clone, PartialEq, Eq)]
pub(super) struct Entry {
    pub key: String,
    pub title: &'static str,
    pub source: &'static str,
    pub kind: &'static str,
    pub status: Status,
    pub revisions: BTreeMap<u32, &'static str>,
}

impl Entry {
    fn new(
        key: &str,
        title: &'static str,
        source: &'static str,
        kind: &'static str,
        status: Status,
        bodies: &[&'static str],
    ) -> Self {
        Self {
            key: key.into(),
            title,
            source,
            kind,
            status,
            revisions: (1..).zip(bodies.iter().copied()).collect(),
        }
    }
    pub fn latest(&self) -> u32 {
        self.revisions.last_key_value().map_or(0, |(&version, _)| version)
    }
}

pub(super) struct Store {
    pub entries: Vec<Entry>,
    pub automatic: bool,
    pub generation: u32,
}

impl Store {
    pub fn seed(project: &str) -> Self {
        let entries = match project {
            "agent-hub" => vec![
                Entry::new(
                    "release-check",
                    "Проверка перед выпуском",
                    "docs/release.md",
                    "Процедура",
                    Status::Active,
                    &[
                        "Собрать пакет для Linux и приложить журнал к задаче.",
                        "Собрать пакет для Linux, затем проверить чистую установку.",
                        "Собрать пакеты для Linux и Windows. Проверить чистую установку и обновление с предыдущего выпуска.",
                    ],
                ),
                Entry::new(
                    "topic-routing",
                    "Как тема выбирает проект",
                    "docs/telegram.md",
                    "Решение",
                    Status::Active,
                    &[
                        "Тема привязывается к каталогу при первом запуске.",
                        "Тема хранит каталог проекта и выбранного агента. Следующее поручение возобновляет ту же сессию.",
                    ],
                ),
                Entry::new(
                    "settings-layout",
                    "Старая схема настроек",
                    "docs/settings.md",
                    "Решение",
                    Status::Stale,
                    &[
                        "Все настройки расположены на одной странице. Пересмотреть после появления разделов.",
                    ],
                ),
                Entry::new(
                    "first-run",
                    "Заметки первого запуска",
                    "notes/pilot.md",
                    "Прогресс",
                    Status::Archived,
                    &[
                        "Первый проход мастера настройки завершён. Замечания перенесены в документацию.",
                    ],
                ),
            ],
            "docs-site" => vec![Entry::new(
                "guide",
                "Структура руководства",
                "docs/index.md",
                "Решение",
                Status::Active,
                &[
                    "Начать руководство с установки и подключения бота.",
                    "После установки показать первый запуск агента и историю поручений.",
                ],
            )],
            _ => Vec::new(),
        };
        Self { entries, automatic: false, generation: 1 }
    }
    pub fn entry(&self, key: &str) -> Option<&Entry> {
        self.entries.iter().find(|entry| entry.key == key)
    }
}

#[derive(Clone)]
pub(super) struct Archive {
    pub from: String,
    pub entries: Vec<Entry>,
}

impl Default for Archive {
    fn default() -> Self {
        Self {
            from: "release-worktree".into(),
            entries: vec![
                Entry::new(
                    "release-check",
                    "Проверка предрелизной сборки",
                    "notes/release.md",
                    "Процедура",
                    Status::Active,
                    &[
                        "Проверить установочный пакет на чистой машине и записать длительность установки.",
                    ],
                ),
                Entry::new(
                    "package-smoke",
                    "Тест установленного пакета",
                    "scripts/smoke",
                    "Процедура",
                    Status::Active,
                    &[
                        "Запустить установленный Agent Hub, открыть настройки и проверить выбор проекта.",
                    ],
                ),
            ],
        }
    }
}

pub(super) struct Projects {
    pub stores: BTreeMap<String, Store>,
    pub bindings: BTreeMap<String, Option<String>>,
    pub archive: Option<Archive>,
}

impl Default for Projects {
    fn default() -> Self {
        let mut result = Self { stores: BTreeMap::new(), bindings: BTreeMap::new(), archive: None };
        for project in ["agent-hub", "docs-site", "sandbox"] {
            result.register(project);
        }
        result
    }
}
impl Projects {
    pub fn register(&mut self, project: &str) {
        if !self.bindings.contains_key(project) {
            self.bindings.insert(project.into(), Some(project.into()));
            self.stores.insert(project.into(), Store::seed(project));
        }
    }
    pub fn storage_key(&self, project: &str) -> Option<&str> {
        self.bindings.get(project)?.as_deref()
    }
    pub fn store(&self, project: &str) -> Option<&Store> {
        self.stores.get(self.storage_key(project)?)
    }
}
