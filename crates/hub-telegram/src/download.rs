//! Attachments of a turn: photos inline for the model, other files into the session's
//! uploads directory.

use hub_core::attachments::{MAX_DOWNLOAD_BYTES, prompt_text, upload_path};
use hub_core::domain::{AbsolutePath, Image, ImageMediaType, Prompt};

use crate::inbound::Turn;
use crate::messenger::SendError;
use crate::paths::{UploadError, prepare_upload};
use crate::sender::Sender;

#[derive(Debug, thiserror::Error)]
pub enum DownloadError {
    #[error("{name}: больше 20 МБ, Telegram не отдаёт боту такие файлы")]
    TooLarge { name: String },
    #[error("Не удалось скачать вложение: {0}")]
    Fetch(SendError),
    #[error(transparent)]
    Store(#[from] UploadError),
    #[error("Сохранение вложения прервано")]
    Interrupted,
    #[error("В сообщении нет ни текста, ни вложений")]
    Empty,
}

pub async fn download(
    sender: &Sender,
    cwd: &AbsolutePath,
    turn: Turn,
) -> Result<Prompt, DownloadError> {
    let Turn { text, photos, files } = turn;
    if let Some(oversized) = photos
        .iter()
        .chain(&files)
        .find(|upload| upload.file.size.is_some_and(|size| size > MAX_DOWNLOAD_BYTES))
    {
        let name = oversized.file.name.clone().unwrap_or_else(|| "фото".to_owned());
        return Err(DownloadError::TooLarge { name });
    }
    let mut images = Vec::with_capacity(photos.len());
    for photo in photos {
        let data = sender.messenger().fetch(photo.file.id).await.map_err(DownloadError::Fetch)?;
        // Telegram re-encodes every photo it stores as JPEG.
        images.push(Image { media: ImageMediaType::Jpeg, data });
    }
    let mut paths = Vec::with_capacity(files.len());
    for upload in files {
        let target = upload_path(cwd.as_path(), upload.message, upload.file.name.as_deref());
        let (root, checked) = (cwd.clone(), target.clone());
        tokio::task::spawn_blocking(move || prepare_upload(&root, &checked))
            .await
            .map_err(|_| DownloadError::Interrupted)??;
        // `create_new` refuses an entry planted between the check and the write.
        let file = tokio::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&target)
            .await
            .map_err(|error| DownloadError::Store(UploadError::Io(error)))?;
        sender.messenger().save(upload.file.id, file).await.map_err(DownloadError::Fetch)?;
        paths.push(target);
    }
    Prompt::new(prompt_text(&text, &paths), images).ok_or(DownloadError::Empty)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use hub_core::domain::MessageId;

    use super::*;
    use crate::inbound::{FileRef, Upload};
    use crate::messenger::Messenger;
    use crate::paths::workspace_root;
    use crate::testing::FakeMessenger;

    fn upload(message: i32, id: &str, name: Option<&str>, size: u64) -> Upload {
        Upload {
            message: MessageId(message),
            file: FileRef { id: id.to_owned(), name: name.map(str::to_owned), size: Some(size) },
        }
    }

    fn sender_with(files: &[(&str, &[u8])]) -> Sender {
        let mut messenger = FakeMessenger::default();
        for (id, bytes) in files {
            messenger.files.insert((*id).to_owned(), bytes.to_vec());
        }
        Sender::new(Arc::new(messenger) as Arc<dyn Messenger>)
    }

    #[tokio::test]
    async fn photos_become_images_and_files_land_in_uploads() {
        let dir = tempfile::tempdir().unwrap();
        let cwd = workspace_root(dir.path()).unwrap();
        let sender = sender_with(&[("p", b"\xff\xd8"), ("d", b"%PDF")]);
        let turn = Turn {
            text: "сравни".to_owned(),
            photos: vec![upload(1, "p", None, 2)],
            files: vec![upload(2, "d", Some("отчёт.pdf"), 4)],
        };

        let prompt = download(&sender, &cwd, turn).await.unwrap();

        let images: Vec<_> = prompt.images().iter().map(|i| (i.media, i.data.clone())).collect();
        assert_eq!(images, [(ImageMediaType::Jpeg, vec![0xff, 0xd8])]);
        let saved = cwd.as_path().join(".agent-hub").join("uploads").join("2-отчёт.pdf");
        assert_eq!(std::fs::read(&saved).unwrap(), b"%PDF");
        assert!(prompt.text().starts_with("сравни\n\nПриложенные файлы:\n- "));
        assert!(prompt.text().ends_with("2-отчёт.pdf"));
    }

    #[tokio::test]
    async fn oversized_attachment_is_refused_before_download() {
        let dir = tempfile::tempdir().unwrap();
        let cwd = workspace_root(dir.path()).unwrap();
        let big = upload(1, "d", Some("big.zip"), 21 * 1024 * 1024);
        let turn = Turn { text: String::new(), photos: Vec::new(), files: vec![big] };
        let error = download(&sender_with(&[]), &cwd, turn).await.unwrap_err();
        assert_eq!(error.to_string(), "big.zip: больше 20 МБ, Telegram не отдаёт боту такие файлы");
    }

    #[tokio::test]
    async fn failed_fetch_is_reported() {
        let dir = tempfile::tempdir().unwrap();
        let cwd = workspace_root(dir.path()).unwrap();
        let turn = Turn {
            text: String::new(),
            photos: vec![upload(1, "gone", None, 2)],
            files: Vec::new(),
        };
        let error = download(&sender_with(&[]), &cwd, turn).await.unwrap_err();
        assert!(error.to_string().starts_with("Не удалось скачать вложение"));
    }
}
