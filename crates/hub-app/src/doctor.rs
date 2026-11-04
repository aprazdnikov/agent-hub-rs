//! Bounded local metadata and network checks; never executes an agent or action.

use futures::future::{BoxFuture, join_all};
use hub_core::{
    doctor::{Check, Report, Target},
    domain::BackendKind,
    projects::Project,
    settings::Settings,
};
use hub_telegram::{
    doctor::DoctorRunner,
    paths::{resolve_cwd, workspace_root},
};
use std::{
    path::{Path, PathBuf},
    sync::{Arc, LazyLock},
    time::Duration,
};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

mod dns;
use dns::Resolver;

const CHECK_TIMEOUT: Duration = Duration::from_secs(5);
// Shared across bot restarts: timed-out filesystem calls still own their permits.
static SLOTS: LazyLock<Arc<Semaphore>> = LazyLock::new(|| Arc::new(Semaphore::new(2)));

pub struct LocalDoctor {
    home: PathBuf,
    slots: Arc<Semaphore>,
}
impl LocalDoctor {
    #[must_use]
    pub fn new(home: PathBuf) -> Self {
        Self { home, slots: Arc::clone(&SLOTS) }
    }
}
impl DoctorRunner for LocalDoctor {
    fn run(
        &self,
        project: Project,
        settings: Arc<Settings>,
    ) -> BoxFuture<'static, Result<Report, String>> {
        let home = self.home.clone();
        let slots = self.slots.clone();
        Box::pin(async move {
            let checks = project.checks().to_vec();
            let resolver = Resolver::default();
            let http_resolver = resolver.clone();
            let ((mut report, client), _permit) = local(slots, move || {
                let directory = workspace_root(&settings.workspace_root)
                    .and_then(|root| resolve_cwd(&root, &home, Some(project.cwd())))
                    .map(|path| path.as_path().display().to_string())
                    .map_err(|error| error.to_string());
                let cli = locate_cli(project.backend(), &settings);
                let client = reqwest::Client::builder()
                    .redirect(reqwest::redirect::Policy::none())
                    .no_proxy()
                    .dns_resolver(Arc::new(http_resolver))
                    .timeout(CHECK_TIMEOUT)
                    .build();
                (Report { directory, cli, resources: Vec::new() }, client)
            })
            .await?;
            let client = client.map_err(|_| "Не удалось подготовить HTTP-клиент".to_owned())?;
            report.resources = join_all(checks.iter().map(|check| async {
                (check.name().to_owned(), network(&client, &resolver, check, CHECK_TIMEOUT).await)
            }))
            .await;
            Ok(report)
        })
    }
}

async fn local<T: Send + 'static>(
    slots: Arc<Semaphore>,
    check: impl FnOnce() -> T + Send + 'static,
) -> Result<(T, OwnedSemaphorePermit), String> {
    let permit = slots
        .try_acquire_owned()
        .map_err(|_| "Локальные проверки ещё заняты. Повторите позже.".to_owned())?;
    tokio::task::spawn_blocking(move || (check(), permit))
        .await
        .map_err(|_| "Ошибка локальной проверки".to_owned())
}

fn locate_cli(backend: BackendKind, settings: &Settings) -> Result<String, String> {
    let path = match backend {
        BackendKind::Claude => hub_claude::version::locate(settings.claude.cli.as_deref()).ok(),
        BackendKind::Codex => hub_codex::version::locate(settings.codex.cli.as_deref()).ok(),
        BackendKind::Qwen => hub_qwen::version::locate(settings.qwen.cli.as_deref()).ok(),
        BackendKind::Hermes => hub_hermes::version::locate(settings.hermes.cli.as_deref()).ok(),
        BackendKind::Opencode => {
            hub_opencode::version::locate(settings.opencode.cli.as_deref()).ok()
        }
    }
    .ok_or_else(|| "CLI не найден в настройках или PATH".to_owned())?;
    executable(&path)?;
    Ok(path.display().to_string())
}

fn executable(path: &Path) -> Result<(), String> {
    let metadata = std::fs::metadata(path).map_err(|_| "Файл CLI недоступен".to_owned())?;
    if !metadata.is_file() {
        return Err("Путь CLI не является файлом".to_owned());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o111 == 0 {
            return Err("У файла CLI нет прав на выполнение".to_owned());
        }
    }
    Ok(())
}

async fn network(
    client: &reqwest::Client,
    resolver: &Resolver,
    check: &Check,
    deadline: Duration,
) -> Result<String, String> {
    tokio::time::timeout(deadline, async {
        match check.target() {
            Target::Http { url, expected } => {
                let response = client.get(url.clone()).send().await.map_err(|error| {
                    if error.is_timeout() {
                        "Тайм-аут".to_owned()
                    } else if error.is_connect() {
                        "Ошибка соединения (DNS, TCP или TLS)".to_owned()
                    } else {
                        "Ошибка HTTP-запроса".to_owned()
                    }
                })?;
                let status = response.status().as_u16();
                if status == *expected {
                    Ok(format!("HTTP {status}"))
                } else {
                    Err(format!("HTTP {status}, ожидался {expected}"))
                }
            }
            Target::Tcp { host, port } => {
                let addresses = resolver.addresses(host, *port).await.map_err(|_| {
                    "Не удалось разрешить имя (DNS занят или недоступен)".to_owned()
                })?;
                tokio::net::TcpStream::connect(addresses.as_slice())
                    .await
                    .map(|_| "TCP-соединение установлено".to_owned())
                    .map_err(|_| "Не удалось подключиться (TCP)".to_owned())
            }
        }
    })
    .await
    .unwrap_or_else(|_| Err("Тайм-аут".to_owned()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use hub_core::{doctor::CheckDraft, projects::ProjectDraft, settings::Draft};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    fn runner(root: &Path) -> LocalDoctor {
        LocalDoctor { home: root.to_path_buf(), slots: Arc::new(Semaphore::new(2)) }
    }

    fn setup(root: &std::path::Path, checks: Vec<CheckDraft>) -> (Project, Arc<Settings>) {
        let cli = root.join("fake-cli");
        std::fs::write(&cli, "must never be executed").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&cli, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        let mut draft = Draft {
            token: "1:a".to_owned(),
            chat: "-100".to_owned(),
            users: "1".to_owned(),
            workspace_root: root.display().to_string(),
            projects: vec![ProjectDraft {
                options: hub_core::options::OptionsDraft::default(),
                id: "p".to_owned(),
                name: "Project".to_owned(),
                cwd: ".".to_owned(),
                backend: "codex".to_owned(),
                actions: vec![],
                checks,
            }],
            ..Draft::default()
        };
        draft.codex.cli = cli.display().to_string();
        let settings = Arc::new(draft.parse(root).unwrap());
        (settings.projects.first().unwrap().clone(), settings)
    }

    async fn http(response: &'static str) -> (String, tokio::task::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/health", listener.local_addr().unwrap());
        let task = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = [0; 2048];
            assert!(socket.read(&mut request).await.unwrap() > 0);
            socket.write_all(response.as_bytes()).await.unwrap();
            // An unfinished body must not delay a headers-only check.
            std::future::pending::<()>().await;
        });
        (url, task)
    }

    #[tokio::test]
    async fn checks_http_headers_and_tcp_without_executing_cli() {
        let dir = tempfile::tempdir().unwrap();
        let tcp = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let (url, server) = http("HTTP/1.1 200 OK\r\nContent-Length: 999999\r\n\r\n").await;
        let checks = vec![
            CheckDraft::Http { name: "Health".to_owned(), url, expected: 200 },
            CheckDraft::Tcp {
                name: "TCP".to_owned(),
                host: "127.0.0.1".to_owned(),
                port: tcp.local_addr().unwrap().port(),
            },
        ];
        let (project, settings) = setup(dir.path(), checks);
        let report = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            runner(dir.path()).run(project, settings),
        )
        .await
        .unwrap()
        .unwrap();
        assert!(report.directory.is_ok());
        assert!(report.cli.is_ok());
        assert!(report.resources.iter().all(|(_, result)| result.is_ok()));
        assert_eq!(report.resources.len(), 2);
        server.abort();
    }

    #[tokio::test]
    async fn redirects_are_reported_without_contacting_the_destination() {
        let dir = tempfile::tempdir().unwrap();
        let destination = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/", listener.local_addr().unwrap());
        let response = format!(
            "HTTP/1.1 302 Found\r\nLocation: http://{}/\r\nContent-Length: 0\r\n\r\n",
            destination.local_addr().unwrap()
        );
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut buf = [0; 2048];
            assert!(socket.read(&mut buf).await.unwrap() > 0);
            socket.write_all(response.as_bytes()).await.unwrap();
        });
        let (project, settings) = setup(
            dir.path(),
            vec![CheckDraft::Http { name: "Health".to_owned(), url, expected: 200 }],
        );
        let report = runner(dir.path()).run(project, settings).await.unwrap();
        assert!(report.resources.first().unwrap().1.as_ref().unwrap_err().contains("302"));
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(30), destination.accept())
                .await
                .is_err()
        );
        server.await.unwrap();
    }
    #[tokio::test]
    async fn failures_and_timeouts_are_bounded_and_do_not_include_urls() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/private-path", listener.local_addr().unwrap());
        let draft = CheckDraft::Http { name: "Hang".to_owned(), url, expected: 200 };
        let checks = hub_core::doctor::parse(&[draft]).unwrap();
        let client = reqwest::Client::builder().no_proxy().build().unwrap();
        let result = network(
            &client,
            &Resolver::default(),
            checks.first().unwrap(),
            Duration::from_millis(30),
        )
        .await;
        assert_eq!(result.unwrap_err(), "Тайм-аут");
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        let drafts =
            [CheckDraft::Tcp { name: "Closed".to_owned(), host: "127.0.0.1".to_owned(), port }];
        let checks = hub_core::doctor::parse(&drafts).unwrap();
        assert!(
            network(&client, &Resolver::default(), checks.first().unwrap(), Duration::from_secs(1))
                .await
                .is_err()
        );
        let (url, server) = http("HTTP/1.1 401 Unauthorized\r\nContent-Length: 0\r\n\r\n").await;
        let checks = hub_core::doctor::parse(&[CheckDraft::Http {
            name: "Auth".to_owned(),
            url,
            expected: 200,
        }])
        .unwrap();
        assert_eq!(
            network(&client, &Resolver::default(), checks.first().unwrap(), Duration::from_secs(1))
                .await
                .unwrap_err(),
            "HTTP 401, ожидался 200"
        );
        server.abort();
    }

    #[tokio::test]
    async fn missing_directory_and_cli_are_reported_independently() {
        let dir = tempfile::tempdir().unwrap();
        let (project, settings) = setup(dir.path(), vec![]);
        std::fs::remove_file(dir.path().join("fake-cli")).unwrap();
        let report = runner(dir.path()).run(project.clone(), settings.clone()).await.unwrap();
        assert!(report.directory.is_ok());
        assert!(report.cli.is_err());
        let mut settings = (*settings).clone();
        settings.workspace_root = dir.path().join("missing");
        let report = runner(dir.path()).run(project, Arc::new(settings)).await.unwrap();
        assert!(report.directory.is_err());
        assert!(report.cli.is_err());
    }

    #[tokio::test]
    async fn cancelled_filesystem_workers_keep_their_slots_until_they_finish() {
        let slots = Arc::new(Semaphore::new(1));
        let (entered, started) = tokio::sync::oneshot::channel();
        let (release, wait) = std::sync::mpsc::channel();
        let task = tokio::spawn(local(slots.clone(), move || {
            entered.send(()).unwrap();
            wait.recv().unwrap();
        }));
        started.await.unwrap();
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        assert!(local(slots.clone(), || ()).await.is_err());
        release.send(()).unwrap();
        tokio::time::timeout(Duration::from_secs(2), async {
            while slots.available_permits() == 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert!(local(slots, || ()).await.is_ok());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn symlink_escape_and_nonexecutable_cli_are_rejected() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(outside.path(), dir.path().join("linked")).unwrap();
        let (_, settings) = setup(dir.path(), vec![]);
        let mut draft = Draft::from_settings(&settings);
        draft.projects.first_mut().unwrap().cwd = "linked".to_owned();
        let settings = Arc::new(draft.parse(dir.path()).unwrap());
        std::fs::set_permissions(
            dir.path().join("fake-cli"),
            std::fs::Permissions::from_mode(0o600),
        )
        .unwrap();
        let report = runner(dir.path())
            .run(settings.projects.first().unwrap().clone(), settings)
            .await
            .unwrap();
        assert!(report.directory.unwrap_err().contains("вне корня"));
        assert!(report.cli.unwrap_err().contains("прав на выполнение"));
    }
}
