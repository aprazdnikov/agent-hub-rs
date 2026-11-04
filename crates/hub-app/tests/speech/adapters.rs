use std::{path::PathBuf, time::Duration};

use hub_app::speech::SpeechTranscriber;
use hub_core::{
    settings::ApiKey,
    speech::{SpeechProvider, SpeechSettings},
};
use hub_telegram::transcription::{SpeechError, Transcriber};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::oneshot,
};
use tokio_util::sync::CancellationToken;

fn api_settings(url: String) -> SpeechSettings {
    SpeechSettings {
        provider: SpeechProvider::Api,
        whisper_path: PathBuf::new(),
        model_path: PathBuf::new(),
        ffmpeg_path: PathBuf::new(),
        api_url: url,
        api_model: "configured-model".into(),
        api_key: ApiKey::parse("dedicated-speech-key"),
    }
}

async fn read_request(stream: &mut TcpStream) -> anyhow::Result<Vec<u8>> {
    let mut request = Vec::new();
    let mut buffer = [0_u8; 1024];
    loop {
        let size = stream.read(&mut buffer).await?;
        if size == 0 {
            anyhow::bail!("Connection closed before request completed");
        }
        request.extend(buffer.iter().take(size));
        anyhow::ensure!(request.len() < 20_000, "Test request exceeded its bound");
        if let Some(end) = request.windows(4).position(|bytes| bytes == b"\r\n\r\n") {
            let headers = String::from_utf8_lossy(request.get(..end).unwrap_or_default());
            let length = headers
                .lines()
                .find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    name.eq_ignore_ascii_case("content-length")
                        .then(|| value.trim().parse::<usize>())
                })
                .transpose()?
                .ok_or_else(|| anyhow::anyhow!("Content-Length missing"))?;
            if request.len() >= end + 4 + length {
                return Ok(request);
            }
        }
    }
}

async fn serve_once(
    response: String,
) -> anyhow::Result<(String, tokio::task::JoinHandle<anyhow::Result<Vec<u8>>>)> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let url = format!("http://{}/v1", listener.local_addr()?);
    let task = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await?;
        let request = read_request(&mut stream).await?;
        stream.write_all(response.as_bytes()).await?;
        Ok(request)
    });
    Ok((url, task))
}

fn response(status: &str, body: &str) -> String {
    format!(
        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
}

#[tokio::test]
async fn api_sends_dedicated_key_ogg_and_model_then_returns_text() -> anyhow::Result<()> {
    let (url, server) = serve_once(response("200 OK", r#"{"text":"  hello, мир  "}"#)).await?;
    let transcript = SpeechTranscriber
        .transcribe(api_settings(url), b"OggS-test-audio".to_vec(), CancellationToken::new())
        .await?;
    let request = String::from_utf8(server.await??)?;
    assert_eq!(transcript, "hello, мир");
    for expected in [
        "POST /v1/audio/transcriptions HTTP/1.1",
        "authorization: Bearer dedicated-speech-key",
        "name=\"model\"",
        "configured-model",
        "name=\"file\"; filename=\"voice.ogg\"",
        "Content-Type: audio/ogg",
        "OggS-test-audio",
        "name=\"response_format\"",
        "json",
    ] {
        assert!(request.contains(expected), "Missing multipart request component: {expected}");
    }
    Ok(())
}

#[rstest::rstest]
#[case::http_error("401 Unauthorized", r#"{"error":"secret-provider-body"}"#.into())]
#[case::invalid_json("200 OK", "secret-provider-body".into())]
#[case::missing_text("200 OK", "{}".into())]
#[case::empty_text("200 OK", r#"{"text":"  "}"#.into())]
#[case::too_much_text("200 OK", serde_json::json!({"text": "😀".repeat(1501)}).to_string())]
#[case::oversize_body("200 OK", "x".repeat(65_537))]
#[tokio::test]
async fn api_rejects_bad_responses_without_exposing_body_or_key(
    #[case] status: &str,
    #[case] body: String,
) -> anyhow::Result<()> {
    let (url, server) = serve_once(response(status, &body)).await?;
    let result =
        SpeechTranscriber.transcribe(api_settings(url), vec![1], CancellationToken::new()).await;
    let SpeechError::Failed(message) =
        result.err().ok_or_else(|| anyhow::anyhow!("Bad response accepted"))?
    else {
        anyhow::bail!("Expected safe provider failure");
    };
    assert!(
        !message.contains("secret-provider-body")
            && !message.contains("dedicated-speech-key")
            && !message.contains("http")
    );
    server.await??;
    Ok(())
}

#[tokio::test]
async fn api_does_not_follow_redirects() -> anyhow::Result<()> {
    let target = TcpListener::bind("127.0.0.1:0").await?;
    let (url, server) = serve_once(format!("HTTP/1.1 307 Temporary Redirect\r\nLocation: http://{}/collect\r\nContent-Length: 0\r\nConnection: close\r\n\r\n", target.local_addr()?)).await?;
    let result =
        SpeechTranscriber.transcribe(api_settings(url), vec![1], CancellationToken::new()).await;
    assert!(matches!(result, Err(SpeechError::Failed(_))));
    assert!(tokio::time::timeout(Duration::from_millis(100), target.accept()).await.is_err());
    server.await??;
    Ok(())
}

#[tokio::test]
async fn api_bounds_chunked_responses_without_a_content_length() -> anyhow::Result<()> {
    let body = "x".repeat(65_537);
    let response = format!(
        "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n{:x}\r\n{body}\r\n0\r\n\r\n",
        body.len()
    );
    let (url, server) = serve_once(response).await?;
    let result =
        SpeechTranscriber.transcribe(api_settings(url), vec![1], CancellationToken::new()).await;
    assert!(matches!(result, Err(SpeechError::Failed(_))));
    server.await??;
    Ok(())
}

#[rstest::rstest]
#[case::remote_cleartext("http://example.com/v1")]
#[case::userinfo("https://name:password@example.com/v1")]
#[case::fragment("https://example.com/v1#fragment")]
#[case::query("https://example.com/v1?secret=key")]
#[tokio::test]
async fn api_rejects_unsafe_urls_before_sending(#[case] url: &str) {
    let result = SpeechTranscriber
        .transcribe(api_settings(url.into()), vec![1], CancellationToken::new())
        .await;
    assert!(matches!(result, Err(SpeechError::Failed(_))));
}

#[tokio::test]
async fn api_cancellation_interrupts_a_waiting_response() -> anyhow::Result<()> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let settings = api_settings(format!("http://{}", listener.local_addr()?));
    let (ready, started) = oneshot::channel();
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await?;
        read_request(&mut stream).await?;
        let _ = ready.send(());
        let mut buffer = [0_u8; 1];
        Ok::<_, anyhow::Error>(stream.read(&mut buffer).await?)
    });
    let cancel = CancellationToken::new();
    let task_cancel = cancel.clone();
    let task =
        tokio::spawn(
            async move { SpeechTranscriber.transcribe(settings, vec![1], task_cancel).await },
        );
    tokio::time::timeout(Duration::from_secs(2), started).await??;
    cancel.cancel();
    assert!(matches!(task.await?, Err(SpeechError::Cancelled)));
    tokio::time::timeout(Duration::from_secs(2), server).await???;
    Ok(())
}

#[tokio::test]
async fn api_timeout_bounds_an_unresponsive_provider() -> anyhow::Result<()> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let settings = api_settings(format!("http://{}", listener.local_addr()?));
    let (ready, started) = oneshot::channel();
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await?;
        read_request(&mut stream).await?;
        let _ = ready.send(());
        let mut buffer = [0_u8; 1];
        Ok::<_, anyhow::Error>(stream.read(&mut buffer).await?)
    });
    let task = tokio::spawn(async move {
        SpeechTranscriber.transcribe(settings, vec![1], CancellationToken::new()).await
    });
    tokio::time::timeout(Duration::from_secs(2), started).await??;
    tokio::time::pause();
    tokio::time::advance(Duration::from_secs(181)).await;
    let result = task.await?;
    tokio::time::resume();
    assert!(matches!(result, Err(SpeechError::Timeout)));
    tokio::time::timeout(Duration::from_secs(2), server).await???;
    Ok(())
}

#[cfg(unix)]
mod local {
    use super::*;
    use std::{fs, os::unix::fs::PermissionsExt, path::Path};

    // Concurrent fork/exec can inherit another fixture's open writable script
    // descriptor until CLOEXEC runs, making the script transiently ETXTBSY.
    // Serialize fixture creation/execution, without retrying production spawns.
    static FIXTURES: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    fn executable(path: &Path, body: &str) -> anyhow::Result<()> {
        fs::write(path, format!("#!/bin/sh\nset -eu\n{body}\n"))?;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
        Ok(())
    }

    fn settings(root: &Path, decoder: &str, whisper: &str) -> anyhow::Result<SpeechSettings> {
        let ffmpeg_path = root.join("fake ffmpeg");
        let whisper_path = root.join("fake whisper");
        let model_path = root.join("model with spaces.bin");
        executable(&ffmpeg_path, &format!("cat >/dev/null\n{decoder}"))?;
        executable(
            &whisper_path,
            &format!("test \"$(head -c 4)\" = RIFF\ncat >/dev/null\n{whisper}"),
        )?;
        fs::write(&model_path, b"model")?;
        Ok(SpeechSettings {
            provider: SpeechProvider::Local,
            whisper_path,
            model_path,
            ffmpeg_path,
            api_url: String::new(),
            api_model: String::new(),
            api_key: None,
        })
    }

    async fn wait_for_file(
        path: &Path,
        task: &mut tokio::task::JoinHandle<Result<String, SpeechError>>,
    ) -> anyhow::Result<()> {
        let ready = tokio::time::timeout(Duration::from_secs(3), async {
            while !path.exists() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        });
        tokio::select! {
            result = task => anyhow::bail!("Local adapter exited before executable became ready: {result:?}"),
            result = ready => result?,
        }
        Ok(())
    }

    #[tokio::test]
    async fn pipes_wav_without_writing_audio_and_cleans_private_directory() -> anyhow::Result<()> {
        let _fixture = FIXTURES.lock().await;
        let root = tempfile::tempdir()?;
        let marker = root.path().join("workdir");
        let options = settings(
            root.path(),
            &format!("pwd > '{}'; printf '\\000\\000\\001\\000'", marker.display()),
            "while [ $# -gt 0 ]; do case \"$1\" in -m) test -f \"$2\"; shift 2;; -f) test \"$2\" = -; shift 2;; -of) test \"$2\" = transcription; shift 2;; -l) test \"$2\" = auto; shift 2;; -np|-nt) shift;; *) exit 8;; esac; done; test -z \"$(ls -A)\"; printf '  local transcript  '",
        )?;
        let transcript = SpeechTranscriber
            .transcribe(options, b"OggS".to_vec(), CancellationToken::new())
            .await?;
        let temporary = fs::read_to_string(marker)?;
        assert_eq!(
            (transcript, Path::new(temporary.trim()).exists()),
            ("local transcript".into(), false)
        );
        Ok(())
    }

    #[tokio::test]
    async fn concurrently_feeds_audio_and_drains_decoder_output() -> anyhow::Result<()> {
        let _fixture = FIXTURES.lock().await;
        let root = tempfile::tempdir()?;
        let options = settings(root.path(), "printf unused", "printf transcript")?;
        // Fill stdout before reading stdin. A runner that writes all stdin first
        // deadlocks once the input and output pipes both fill.
        executable(
            &options.ffmpeg_path,
            "head -c 131072 /dev/zero; test \"$(wc -c)\" -eq 1048576",
        )?;
        let result = tokio::time::timeout(
            Duration::from_secs(2),
            SpeechTranscriber.transcribe(options, vec![1; 1024 * 1024], CancellationToken::new()),
        )
        .await??;
        assert_eq!(result, "transcript");
        Ok(())
    }

    #[rstest::rstest]
    #[case::duration("head -c 5760002 /dev/zero", "printf accepted")]
    #[case::decode_error("printf private-error >&2; exit 7", "printf accepted")]
    #[case::whisper_error("printf '\\000\\000'", "printf private-error >&2; exit 8")]
    #[case::huge_transcript("printf '\\000\\000'", "head -c 20000 /dev/zero")]
    #[tokio::test]
    async fn rejects_invalid_local_output_and_cleans_files(
        #[case] decoder: &str,
        #[case] whisper: &str,
    ) -> anyhow::Result<()> {
        let _fixture = FIXTURES.lock().await;
        let root = tempfile::tempdir()?;
        let marker = root.path().join("workdir");
        let options =
            settings(root.path(), &format!("pwd > '{}'; {decoder}", marker.display()), whisper)?;
        let result = SpeechTranscriber.transcribe(options, vec![1], CancellationToken::new()).await;
        assert!(matches!(result, Err(SpeechError::Failed(_))));
        assert!(marker.exists(), "Decoder never started: {result:?}");
        assert!(!Path::new(fs::read_to_string(marker)?.trim()).exists());
        Ok(())
    }

    #[rstest::rstest]
    #[case::cancel(false)]
    #[case::drop_future(true)]
    #[tokio::test]
    async fn interrupted_process_tree_cannot_survive_and_private_files_are_removed(
        #[case] abort: bool,
    ) -> anyhow::Result<()> {
        let _fixture = FIXTURES.lock().await;
        let root = tempfile::tempdir()?;
        let marker = root.path().join("workdir");
        let survived = root.path().join("survived");
        let options = settings(
            root.path(),
            &format!(
                "pwd > '{}'; sh -c 'sleep 0.4; echo survived > \"{}\"; sleep 30' & wait",
                marker.display(),
                survived.display()
            ),
            "printf accepted",
        )?;
        let cancel = CancellationToken::new();
        let task_cancel = cancel.clone();
        let mut task = tokio::spawn(async move {
            SpeechTranscriber.transcribe(options, vec![1], task_cancel).await
        });
        wait_for_file(&marker, &mut task).await?;
        assert!(fs::read_dir(fs::read_to_string(&marker)?.trim())?.next().is_none());
        if abort {
            task.abort();
            assert!(task.await.is_err_and(|error| error.is_cancelled()));
        } else {
            cancel.cancel();
            assert!(matches!(task.await?, Err(SpeechError::Cancelled)));
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
        assert!(!survived.exists() && !Path::new(fs::read_to_string(marker)?.trim()).exists());
        Ok(())
    }

    #[tokio::test]
    async fn timeout_stops_local_processes_and_removes_private_files() -> anyhow::Result<()> {
        let _fixture = FIXTURES.lock().await;
        let root = tempfile::tempdir()?;
        let marker = root.path().join("workdir");
        let survived = root.path().join("survived");
        let options = settings(
            root.path(),
            &format!(
                "pwd > '{}'; sh -c 'sleep 0.4; echo survived > \"{}\"; sleep 30' & wait",
                marker.display(),
                survived.display()
            ),
            "printf accepted",
        )?;
        let mut task = tokio::spawn(async move {
            SpeechTranscriber.transcribe(options, vec![1], CancellationToken::new()).await
        });
        wait_for_file(&marker, &mut task).await?;
        tokio::time::pause();
        tokio::time::advance(Duration::from_secs(181)).await;
        let result = task.await?;
        tokio::time::resume();
        assert!(matches!(result, Err(SpeechError::Timeout)));
        tokio::time::sleep(Duration::from_millis(500)).await;
        assert!(!survived.exists() && !Path::new(fs::read_to_string(marker)?.trim()).exists());
        Ok(())
    }
}
