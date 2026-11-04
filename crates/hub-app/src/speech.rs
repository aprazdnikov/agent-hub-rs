//! Bounded speech adapters. Audio and transcripts are never logged.
//!
//! Local recognition runs the configured foreground executables without a shell.
//! It decodes at most three minutes of mono 16 kHz, 16-bit PCM and rejects longer
//! audio rather than silently truncating it. Input audio and a bounded WAV pass
//! through pipes and memory, with an empty private working directory. No model
//! or executable is downloaded and no audio or transcript is written to disk.
//! Unix process groups / Windows jobs are killed on completion, cancellation,
//! timeout, or future drop. Deliberately detached Unix descendants are outside
//! process-group containment, as with the application's action runner.

use std::{
    fs, io,
    net::IpAddr,
    path::{Component, Path, PathBuf},
    process::Stdio,
    time::Duration,
};

use futures::future::BoxFuture;
use hub_core::speech::{SpeechProvider, SpeechSettings};
use hub_telegram::transcription::{
    MAX_AUDIO_BYTES, MAX_DURATION, MAX_TEXT_UNITS, SpeechError, TIMEOUT, Transcriber,
};
use process_wrap::tokio::{ChildWrapper, CommandWrap, KillOnDrop};
use reqwest::{Client, Url, multipart};
use serde::Deserialize;
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWriteExt},
    process::Command,
};
use tokio_util::sync::CancellationToken;

const RESPONSE_BYTES: usize = 64 * 1024;
const TEXT_BYTES: usize = MAX_TEXT_UNITS * 4;
const SAMPLE_RATE: u32 = 16_000;
const PCM_BYTES_PER_SECOND: u32 = SAMPLE_RATE * 2;
const DRAIN_TIMEOUT: Duration = Duration::from_millis(500);

/// Local Whisper.cpp or a configured OpenAI-compatible transcription endpoint.
#[derive(Default)]
pub struct SpeechTranscriber;

impl Transcriber for SpeechTranscriber {
    fn transcribe(
        &self,
        settings: SpeechSettings,
        audio: Vec<u8>,
        cancel: CancellationToken,
    ) -> BoxFuture<'_, Result<String, SpeechError>> {
        Box::pin(async move {
            if cancel.is_cancelled() {
                return Err(SpeechError::Cancelled);
            }
            if settings.provider == SpeechProvider::Disabled {
                return Err(SpeechError::Disabled);
            }
            if audio.is_empty() || audio.len() > MAX_AUDIO_BYTES {
                return Err(failed("Голосовое сообщение пустое или превышает 10 МиБ."));
            }
            let recognize = async {
                match settings.provider {
                    SpeechProvider::Disabled => Err(SpeechError::Disabled),
                    SpeechProvider::Local => local(&settings, &audio).await,
                    SpeechProvider::Api => api(&settings, audio).await,
                }
            };
            tokio::select! {
                biased;
                () = cancel.cancelled() => Err(SpeechError::Cancelled),
                result = tokio::time::timeout(TIMEOUT, recognize) => result.unwrap_or(Err(SpeechError::Timeout)),
            }
        })
    }
}

fn failed(message: &str) -> SpeechError {
    SpeechError::Failed(message.into())
}

fn transcript(text: &str) -> Result<String, SpeechError> {
    let text = text.trim();
    if text.is_empty() {
        return Err(failed("Речь не распознана. Попробуйте другое голосовое сообщение или текст."));
    }
    if text.encode_utf16().count() > MAX_TEXT_UNITS {
        return Err(failed(
            "Результат распознавания слишком длинный. Отправьте более короткое сообщение.",
        ));
    }
    Ok(text.into())
}

async fn api(settings: &SpeechSettings, audio: Vec<u8>) -> Result<String, SpeechError> {
    let endpoint = api_endpoint(&settings.api_url)?;
    let key = settings
        .api_key
        .as_ref()
        .ok_or_else(|| failed("Укажите отдельный API-ключ распознавания речи в настройках."))?;
    if settings.api_model.trim().is_empty()
        || settings.api_model.len() > 256
        || settings
            .api_model
            .chars()
            .any(|character| character.is_whitespace() || character.is_control())
    {
        return Err(failed("Укажите корректное имя модели распознавания в настройках."));
    }
    let mut builder = Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .retry(reqwest::retry::never())
        .connect_timeout(Duration::from_secs(15))
        .timeout(TIMEOUT);
    if endpoint.scheme() == "http" {
        // A loopback provider must not send its audio/key through an environment proxy.
        builder = builder.no_proxy();
        if endpoint.host_str() == Some("localhost") {
            builder = builder.resolve_to_addrs(
                "localhost",
                &[
                    std::net::SocketAddr::from(([127, 0, 0, 1], 0)),
                    std::net::SocketAddr::from((std::net::Ipv6Addr::LOCALHOST, 0)),
                ],
            );
        }
    }
    let client = builder
        .build()
        .map_err(|_| failed("Не удалось подготовить подключение к сервису распознавания."))?;
    let file = multipart::Part::bytes(audio)
        .file_name("voice.ogg")
        .mime_str("audio/ogg")
        .map_err(|_| failed("Не удалось подготовить голосовое сообщение."))?;
    let form = multipart::Form::new()
        .text("model", settings.api_model.clone())
        .text("response_format", "json")
        .part("file", file);
    let mut response = client
        .post(endpoint)
        .bearer_auth(key.expose())
        .multipart(form)
        .send()
        .await
        .map_err(|error| request_error(&error))?;
    if !response.status().is_success() {
        // Error bodies may echo keys, URLs, audio, or transcripts. Never expose them.
        return Err(failed(
            "Сервис распознавания отклонил запрос. Проверьте настройки и доступность сервиса.",
        ));
    }
    if response.content_length().is_some_and(|size| size > RESPONSE_BYTES as u64) {
        return Err(failed("Ответ сервиса распознавания слишком большой."));
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|error| request_error(&error))? {
        if chunk.len() > RESPONSE_BYTES.saturating_sub(bytes.len()) {
            return Err(failed("Ответ сервиса распознавания слишком большой."));
        }
        bytes.extend_from_slice(&chunk);
    }
    let result: ApiTranscript = serde_json::from_slice(&bytes)
        .map_err(|_| failed("Сервис распознавания вернул некорректный ответ."))?;
    transcript(&result.text)
}

#[derive(Deserialize)]
struct ApiTranscript {
    text: String,
}

fn request_error(error: &reqwest::Error) -> SpeechError {
    if error.is_timeout() {
        SpeechError::Timeout
    } else {
        // reqwest errors contain request URLs; retain no raw error text.
        failed("Не удалось связаться с сервисом распознавания. Проверьте настройки и подключение.")
    }
}

fn api_endpoint(raw: &str) -> Result<Url, SpeechError> {
    let invalid = || {
        failed(
            "Укажите HTTPS-адрес API или HTTP-адрес на этом компьютере без пароля, параметров и фрагмента.",
        )
    };
    let mut url = Url::parse(raw).map_err(|_| invalid())?;
    let loopback = url.host_str().is_some_and(|host| {
        host == "localhost"
            || host.trim_matches(['[', ']']).parse::<IpAddr>().is_ok_and(|ip| ip.is_loopback())
    });
    if url.cannot_be_a_base()
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || !(url.scheme() == "https" || url.scheme() == "http" && loopback)
    {
        return Err(invalid());
    }
    url.set_path(&format!("{}/audio/transcriptions", url.path().trim_end_matches('/')));
    Ok(url)
}

async fn local(settings: &SpeechSettings, audio: &[u8]) -> Result<String, SpeechError> {
    let ffmpeg = executable_path(&settings.ffmpeg_path)?;
    let whisper = executable_path(&settings.whisper_path)?;
    let model = fs::canonicalize(&settings.model_path)
        .map_err(|_| failed("Файл модели Whisper.cpp не найден. Проверьте путь в настройках."))?;
    if !model.is_file() {
        return Err(failed("Выберите файл модели Whisper.cpp в настройках."));
    }
    let temporary = tempfile::Builder::new()
        .prefix("agent-hub-speech-")
        .tempdir()
        .map_err(|_| failed("Не удалось создать временную папку для распознавания."))?;
    let result = local_processes(&ffmpeg, &whisper, &model, temporary.path(), audio).await;
    temporary.close().map_err(|_| failed("Не удалось удалить временные файлы распознавания."))?;
    result
}

async fn local_processes(
    ffmpeg: &Path,
    whisper: &Path,
    model: &Path,
    directory: &Path,
    audio: &[u8],
) -> Result<String, SpeechError> {
    let mut decode = Command::new(ffmpeg);
    decode
        .args([
            "-nostdin",
            "-hide_banner",
            "-loglevel",
            "error",
            "-protocol_whitelist",
            "pipe",
            "-f",
            "ogg",
            "-i",
            "pipe:0",
        ])
        .args([
            "-map",
            "0:a:0",
            "-vn",
            "-ac",
            "1",
            "-ar",
            "16000",
            "-c:a",
            "pcm_s16le",
            "-f",
            "s16le",
            "pipe:1",
        ]);
    let max_pcm = usize::try_from(MAX_DURATION.as_secs() * u64::from(PCM_BYTES_PER_SECOND))
        .map_err(|_| failed("Не удалось определить ограничение длительности аудио."))?;
    let pcm = run(decode, directory, audio, max_pcm).await?;
    if pcm.is_empty() || !pcm.len().is_multiple_of(2) {
        return Err(failed("Не удалось декодировать голосовое сообщение."));
    }
    let wav = wave(&pcm)?;
    drop(pcm);
    let mut recognize = Command::new(whisper);
    // A non-dash output basename keeps whisper.cpp's live text callback active
    // for stdin input. No -otxt/-oj/etc. is set, so it creates no output files.
    recognize.arg("-m").arg(model).args([
        "-f",
        "-",
        "-of",
        "transcription",
        "-l",
        "auto",
        "-np",
        "-nt",
    ]);
    let output = run(recognize, directory, &wav, TEXT_BYTES).await?;
    transcript(
        std::str::from_utf8(&output)
            .map_err(|_| failed("Whisper.cpp вернул некорректный текст."))?,
    )
}

fn executable_path(path: &Path) -> Result<PathBuf, SpeechError> {
    let invalid = || failed("Укажите исполняемые файлы FFmpeg и Whisper.cpp в настройках.");
    if path.as_os_str().is_empty() {
        return Err(invalid());
    }
    let mut parts = path.components();
    let program = if let (Some(Component::Normal(name)), None) = (parts.next(), parts.next()) {
        if name.to_string_lossy().starts_with('-') {
            return Err(invalid());
        }
        // A bare executable name is resolved through PATH. All other paths are
        // absolute before entering the private working directory.
        path.to_owned()
    } else {
        fs::canonicalize(path).map_err(|_| invalid())?
    };
    #[cfg(windows)]
    if program.extension().and_then(std::ffi::OsStr::to_str).is_some_and(|extension| {
        extension.eq_ignore_ascii_case("cmd") || extension.eq_ignore_ascii_case("bat")
    }) {
        // Windows runs batch files through cmd.exe even when Command is used.
        // Speech providers must be executables, never shell command scripts.
        return Err(invalid());
    }
    Ok(program)
}

fn wave(pcm: &[u8]) -> Result<Vec<u8>, SpeechError> {
    let length =
        u32::try_from(pcm.len()).map_err(|_| failed("Декодированное аудио слишком большое."))?;
    let mut wav = Vec::with_capacity(pcm.len() + 44);
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&(36 + length).to_le_bytes());
    wav.extend_from_slice(b"WAVEfmt ");
    wav.extend_from_slice(&16_u32.to_le_bytes());
    wav.extend_from_slice(&1_u16.to_le_bytes()); // PCM
    wav.extend_from_slice(&1_u16.to_le_bytes()); // mono
    wav.extend_from_slice(&SAMPLE_RATE.to_le_bytes());
    wav.extend_from_slice(&PCM_BYTES_PER_SECOND.to_le_bytes());
    wav.extend_from_slice(&2_u16.to_le_bytes()); // block alignment
    wav.extend_from_slice(&16_u16.to_le_bytes());
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&length.to_le_bytes());
    wav.extend_from_slice(pcm);
    Ok(wav)
}

async fn run(
    mut command: Command,
    directory: &Path,
    input: &[u8],
    limit: usize,
) -> Result<Vec<u8>, SpeechError> {
    command
        .current_dir(directory)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let mut command = CommandWrap::from(command);
    command.wrap(KillOnDrop);
    #[cfg(unix)]
    command.wrap(process_wrap::tokio::ProcessGroup::leader());
    #[cfg(windows)]
    command
        .wrap(process_wrap::tokio::CreationFlags(
            windows::Win32::System::Threading::CREATE_NO_WINDOW,
        ))
        .wrap(process_wrap::tokio::JobObject);
    let child = command.spawn().map_err(|error| {
        failed(&format!(
            "Не удалось запустить FFmpeg или Whisper.cpp. Проверьте установку и пути. ({:?}, код ОС {:?})",
            error.kind(), error.raw_os_error()
        ))
    })?;
    let mut tree = ProcessTree { child, armed: true };
    let stdout = tree
        .child
        .stdout()
        .take()
        .ok_or_else(|| failed("Не удалось прочитать результат распознавания."))?;
    let mut stdin = tree
        .child
        .stdin()
        .take()
        .ok_or_else(|| failed("Не удалось передать аудио процессу распознавания."))?;
    // Drive both pipes together: an executable may produce output before it has
    // consumed all input, including while ffmpeg decodes a larger OGG stream.
    let mut output = std::pin::pin!(async {
        let send = async {
            stdin
                .write_all(input)
                .await
                .map_err(|_| failed("Не удалось передать аудио процессу распознавания."))?;
            drop(stdin);
            Ok::<_, SpeechError>(())
        };
        let ((), bytes) = tokio::try_join!(send, read_bounded(stdout, limit))?;
        Ok::<_, SpeechError>(bytes)
    });
    let (status, bytes) = tokio::select! {
        bytes = &mut output => {
            let bytes = bytes?;
            (tree.child.inner_mut().wait().await, bytes)
        },
        status = tree.child.inner_mut().wait() => {
            // Stop descendants even when the foreground executable exits first.
            tree.stop().map_err(|_| failed("Не удалось остановить процессы распознавания."))?;
            let bytes = tokio::time::timeout(DRAIN_TIMEOUT, &mut output).await
                .map_err(|_| failed("Не удалось завершить чтение результата распознавания."))??;
            (status, bytes)
        },
    };
    tree.stop().map_err(|_| failed("Не удалось остановить процессы распознавания."))?;
    if !status.map_err(|_| failed("Не удалось дождаться завершения распознавания."))?.success()
    {
        return Err(failed(
            "FFmpeg или Whisper.cpp завершился с ошибкой. Проверьте аудио, модель и установку.",
        ));
    }
    Ok(bytes)
}

async fn read_bounded(
    mut reader: impl AsyncRead + Unpin,
    limit: usize,
) -> Result<Vec<u8>, SpeechError> {
    let mut bytes = Vec::new();
    let mut buffer = [0_u8; 8192];
    loop {
        let size = reader
            .read(&mut buffer)
            .await
            .map_err(|_| failed("Не удалось прочитать результат распознавания."))?;
        if size == 0 {
            return Ok(bytes);
        }
        if size > limit.saturating_sub(bytes.len()) {
            return Err(failed(
                "Аудио длиннее 3 минут или результат распознавания слишком большой.",
            ));
        }
        bytes.extend(buffer.iter().take(size));
    }
}

struct ProcessTree {
    child: Box<dyn ChildWrapper>,
    armed: bool,
}

impl ProcessTree {
    fn stop(&mut self) -> io::Result<()> {
        if !self.armed {
            return Ok(());
        }
        let result = self.child.start_kill();
        #[cfg(unix)]
        let result = result.or_else(|error| {
            if error.raw_os_error() == Some(nix::errno::Errno::ESRCH as i32) {
                Ok(())
            } else {
                Err(error)
            }
        });
        if result.is_ok() {
            self.armed = false;
        }
        result
    }
}

impl Drop for ProcessTree {
    fn drop(&mut self) {
        if let Err(error) = self.stop() {
            tracing::warn!(kind = ?error.kind(), os_code = error.raw_os_error(), "Could not stop speech process tree on drop");
        }
    }
}
