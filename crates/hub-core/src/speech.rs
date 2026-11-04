//! Speech recognition configuration shared by the settings form and providers.

use std::fmt;
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::settings::{ApiKey, Field, FieldError, SpeechField};
use crate::workspace::expand_home;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SpeechProvider {
    #[default]
    Disabled,
    Local,
    Api,
}

impl SpeechProvider {
    pub const ALL: [Self; 3] = [Self::Disabled, Self::Local, Self::Api];

    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Disabled => "Выключено",
            Self::Local => "Локально — Whisper.cpp",
            Self::Api => "OpenAI-совместимый API",
        }
    }
}

/// Both providers' configuration is retained when speech recognition is disabled.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpeechSettings {
    pub provider: SpeechProvider,
    pub whisper_path: PathBuf,
    pub model_path: PathBuf,
    pub ffmpeg_path: PathBuf,
    pub api_url: String,
    pub api_model: String,
    pub api_key: Option<ApiKey>,
}

impl Default for SpeechSettings {
    fn default() -> Self {
        Self {
            provider: SpeechProvider::Disabled,
            whisper_path: PathBuf::from("whisper-cli"),
            model_path: PathBuf::new(),
            ffmpeg_path: PathBuf::from("ffmpeg"),
            api_url: "https://api.openai.com/v1".to_owned(),
            api_model: "whisper-1".to_owned(),
            api_key: None,
        }
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct SpeechDraft {
    pub provider: SpeechProvider,
    pub whisper_path: String,
    pub model_path: String,
    pub ffmpeg_path: String,
    pub api_url: String,
    pub api_model: String,
    pub api_key: String,
}

impl Default for SpeechDraft {
    fn default() -> Self {
        Self::from(&SpeechSettings::default())
    }
}

impl fmt::Debug for SpeechDraft {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let Self {
            provider,
            whisper_path,
            model_path,
            ffmpeg_path,
            api_url: _,
            api_model,
            api_key: _,
        } = self;
        f.debug_struct("SpeechDraft")
            .field("provider", provider)
            .field("whisper_path", whisper_path)
            .field("model_path", model_path)
            .field("ffmpeg_path", ffmpeg_path)
            // An unvalidated URL can contain a pasted credential.
            .field("api_url", &"***")
            .field("api_model", api_model)
            .field("api_key", &"***")
            .finish()
    }
}

impl From<&SpeechSettings> for SpeechDraft {
    fn from(settings: &SpeechSettings) -> Self {
        let SpeechSettings {
            provider,
            whisper_path,
            model_path,
            ffmpeg_path,
            api_url,
            api_model,
            api_key,
        } = settings;
        Self {
            provider: *provider,
            whisper_path: whisper_path.display().to_string(),
            model_path: model_path.display().to_string(),
            ffmpeg_path: ffmpeg_path.display().to_string(),
            api_url: api_url.clone(),
            api_model: api_model.clone(),
            api_key: api_key.as_ref().map(|key| key.expose().to_owned()).unwrap_or_default(),
        }
    }
}

impl SpeechDraft {
    pub(crate) fn parse(&self, home: &Path) -> Result<SpeechSettings, Vec<FieldError>> {
        let Self { provider, whisper_path, model_path, ffmpeg_path, api_url, api_model, api_key } =
            self;
        let mut errors = Vec::new();
        let mut record = |field, message: &str| {
            errors.push(FieldError { field: Field::Speech(field), message: message.to_owned() });
        };
        let api_key = if api_key.trim().is_empty() {
            if *provider == SpeechProvider::Api {
                record(SpeechField::ApiKey, "Укажите API-ключ распознавания речи");
            }
            None
        } else if api_key.chars().any(char::is_control) {
            record(SpeechField::ApiKey, "Ключ не должен содержать управляющих символов");
            None
        } else {
            let key = ApiKey::parse(api_key);
            if key.is_none() {
                record(SpeechField::ApiKey, "Ключ не должен содержать пробелов");
            }
            key
        };
        let api_url = if api_url.trim().is_empty() && *provider != SpeechProvider::Api {
            String::new()
        } else {
            match parse_api_url(api_url) {
                Ok(url) => url,
                Err(message) => {
                    record(SpeechField::ApiUrl, message);
                    String::new()
                }
            }
        };
        let api_model = api_model.trim().to_owned();
        if *provider == SpeechProvider::Api
            && (api_model.is_empty()
                || api_model.chars().any(|c| c.is_whitespace() || c.is_control()))
        {
            record(SpeechField::ApiModel, "Укажите имя модели без пробелов");
        }
        let local_path = |raw: &str, executable| {
            if *provider == SpeechProvider::Local {
                parse_local_path(raw, home, executable)
            } else {
                Ok(PathBuf::from(raw.trim()))
            }
        };
        let whisper_path = local_path(whisper_path, true).map_err(|message| {
            record(SpeechField::WhisperPath, message);
        });
        let model_path = local_path(model_path, false).map_err(|message| {
            record(SpeechField::ModelPath, message);
        });
        let ffmpeg_path = local_path(ffmpeg_path, true).map_err(|message| {
            record(SpeechField::FfmpegPath, message);
        });
        match (whisper_path, model_path, ffmpeg_path) {
            (Ok(whisper_path), Ok(model_path), Ok(ffmpeg_path)) if errors.is_empty() => {
                Ok(SpeechSettings {
                    provider: *provider,
                    whisper_path,
                    model_path,
                    ffmpeg_path,
                    api_url,
                    api_model,
                    api_key,
                })
            }
            _ => Err(errors),
        }
    }
}

fn parse_local_path(raw: &str, home: &Path, executable: bool) -> Result<PathBuf, &'static str> {
    let raw = raw.trim();
    if raw.is_empty() || raw.chars().any(char::is_control) {
        return Err("Укажите путь к файлу");
    }
    let path = expand_home(raw, home);
    if path.file_name().is_none() {
        return Err("Укажите путь к файлу");
    }
    if path.is_absolute()
        || (executable
            && !raw.contains(['/', '\\'])
            && !raw.chars().any(char::is_whitespace)
            && matches!(path.components().next(), Some(Component::Normal(_))))
    {
        Ok(path)
    } else if executable {
        Err("Укажите имя команды из PATH, абсолютный путь или путь от ~")
    } else {
        Err("Укажите абсолютный путь к модели или путь от ~")
    }
}

fn parse_api_url(raw: &str) -> Result<String, &'static str> {
    const INVALID: &str = "Укажите HTTPS URL без логина, пароля, query и fragment; HTTP разрешён только для localhost и loopback IP";
    let raw = raw.trim();
    if raw.chars().any(|c| c.is_whitespace() || c.is_control()) || raw.contains('\\') {
        return Err(INVALID);
    }
    let url = url::Url::parse(raw).map_err(|_| INVALID)?;
    let authority = raw
        .split_once("://")
        .map(|(_, rest)| rest.split('/').next().unwrap_or(rest))
        .ok_or(INVALID)?;
    if authority.is_empty()
        || authority.contains('@')
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(INVALID);
    }
    if url.host().is_none() {
        return Err(INVALID);
    }
    // Inspect the literal host: the URL parser also accepts numeric aliases such as 127.1.
    let host = if let Some(bracketed) = authority.strip_prefix('[') {
        bracketed.split_once(']').map(|(host, _)| host).ok_or(INVALID)?
    } else {
        authority.split_once(':').map_or(authority, |(host, _)| host)
    };
    let loopback = host.eq_ignore_ascii_case("localhost")
        || host.parse::<std::net::IpAddr>().is_ok_and(|ip| ip.is_loopback());
    if url.scheme() != "https" && !(url.scheme() == "http" && loopback) {
        return Err(INVALID);
    }
    Ok(url.as_str().trim_end_matches('/').to_owned())
}

/// Persistent, non-secret provider configuration; the key lives only in the keyring.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SpeechFile {
    pub provider: SpeechProvider,
    pub whisper_path: String,
    pub model_path: String,
    pub ffmpeg_path: String,
    pub api_url: String,
    pub api_model: String,
}

impl Default for SpeechFile {
    fn default() -> Self {
        Self::from(&SpeechSettings::default())
    }
}

impl From<&SpeechSettings> for SpeechFile {
    fn from(settings: &SpeechSettings) -> Self {
        let SpeechSettings {
            provider,
            whisper_path,
            model_path,
            ffmpeg_path,
            api_url,
            api_model,
            api_key: _,
        } = settings;
        Self {
            provider: *provider,
            whisper_path: whisper_path.display().to_string(),
            model_path: model_path.display().to_string(),
            ffmpeg_path: ffmpeg_path.display().to_string(),
            api_url: api_url.clone(),
            api_model: api_model.clone(),
        }
    }
}

impl SpeechFile {
    pub(crate) fn to_draft(&self, api_key: String) -> SpeechDraft {
        let Self { provider, whisper_path, model_path, ffmpeg_path, api_url, api_model } = self;
        SpeechDraft {
            provider: *provider,
            whisper_path: whisper_path.clone(),
            model_path: model_path.clone(),
            ffmpeg_path: ffmpeg_path.clone(),
            api_url: api_url.clone(),
            api_model: api_model.clone(),
            api_key,
        }
    }
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;

    fn home() -> PathBuf {
        PathBuf::from(if cfg!(windows) { r"C:\Users\me" } else { "/home/me" })
    }

    #[rstest]
    #[case("https://api.openai.com/v1///", "https://api.openai.com/v1")]
    #[case("https://speech.example", "https://speech.example")]
    #[case("http://localhost:8080/v1/", "http://localhost:8080/v1")]
    #[case("http://127.0.0.2:8080", "http://127.0.0.2:8080")]
    #[case("http://[::1]:8080/v1", "http://[::1]:8080/v1")]
    fn speech_api_url_accepts_secure_or_literal_loopback_endpoints(
        #[case] raw: &str,
        #[case] expected: &str,
    ) {
        assert_eq!(parse_api_url(raw), Ok(expected.to_owned()));
    }

    #[rstest]
    #[case("")]
    #[case("http://speech.example/v1")]
    #[case("http://localhost.example/v1")]
    #[case("http://192.168.1.1/v1")]
    #[case("http://127.1/v1")]
    #[case("http://2130706433/v1")]
    #[case("http://%6cocalhost/v1")]
    #[case("http:////localhost/v1")]
    #[case("ftp://localhost/v1")]
    #[case("https://user:password@example.com/v1")]
    #[case("https://@example.com/v1")]
    #[case("https://example.com/v1?key=secret")]
    #[case("https://example.com/v1#secret")]
    #[case("https://example.com/with space")]
    #[case("https://example.com/\u{0000}")]
    #[case("https://example.com\\@other.example/v1")]
    fn speech_api_url_rejects_unsafe_or_ambiguous_endpoints(#[case] raw: &str) {
        assert!(parse_api_url(raw).is_err());
    }

    #[test]
    fn speech_local_requires_model_but_accepts_path_commands_without_probing() {
        let mut draft = SpeechDraft { provider: SpeechProvider::Local, ..SpeechDraft::default() };
        let errors = draft.parse(&home()).unwrap_err();
        assert_eq!(
            errors.iter().map(|error| error.field).collect::<Vec<_>>(),
            [Field::Speech(SpeechField::ModelPath)]
        );
        draft.model_path = "~/models/ggml-base.bin".to_owned();
        let parsed = draft.parse(&home()).unwrap();
        assert_eq!(parsed.model_path, home().join("models/ggml-base.bin"));
        assert_eq!(parsed.whisper_path, PathBuf::from("whisper-cli"));
        assert_eq!(parsed.ffmpeg_path, PathBuf::from("ffmpeg"));
    }

    #[test]
    fn speech_api_does_not_require_local_paths() {
        let draft = SpeechDraft {
            provider: SpeechProvider::Api,
            whisper_path: String::new(),
            model_path: String::new(),
            ffmpeg_path: String::new(),
            api_key: "sk-speech".to_owned(),
            ..SpeechDraft::default()
        };
        assert!(draft.parse(&home()).is_ok());
    }

    #[test]
    fn speech_api_requires_key_model_and_url() {
        let draft = SpeechDraft {
            provider: SpeechProvider::Api,
            api_url: String::new(),
            api_model: String::new(),
            ..SpeechDraft::default()
        };
        let fields = draft
            .parse(&home())
            .unwrap_err()
            .into_iter()
            .map(|error| error.field)
            .collect::<Vec<_>>();
        assert_eq!(
            fields,
            [
                Field::Speech(SpeechField::ApiKey),
                Field::Speech(SpeechField::ApiUrl),
                Field::Speech(SpeechField::ApiModel)
            ]
        );
    }

    #[rstest]
    #[case(SpeechProvider::Disabled)]
    #[case(SpeechProvider::Local)]
    #[case(SpeechProvider::Api)]
    fn speech_keys_and_urls_are_validated_even_when_inactive(#[case] provider: SpeechProvider) {
        let draft = SpeechDraft {
            provider,
            model_path: home().join("model.bin").display().to_string(),
            api_url: "https://example.com?key=secret".to_owned(),
            api_key: "secret with spaces".to_owned(),
            ..SpeechDraft::default()
        };
        assert_eq!(
            draft
                .parse(&home())
                .unwrap_err()
                .into_iter()
                .map(|error| error.field)
                .collect::<Vec<_>>(),
            [Field::Speech(SpeechField::ApiKey), Field::Speech(SpeechField::ApiUrl)]
        );
        assert!(!format!("{draft:?}").contains("secret"));
    }

    #[test]
    fn speech_switching_provider_preserves_settings_and_redacts_key() {
        let mut draft = SpeechDraft {
            model_path: home().join("models/model.bin").display().to_string(),
            api_key: "sk-speech-secret".to_owned(),
            ..SpeechDraft::default()
        };
        for provider in [SpeechProvider::Api, SpeechProvider::Local, SpeechProvider::Disabled] {
            draft.provider = provider;
            let settings = draft.parse(&home()).unwrap();
            let restored = SpeechDraft::from(&settings);
            assert_eq!(restored, draft);
            assert!(!format!("{draft:?} {settings:?}").contains("sk-speech-secret"));
            let file = SpeechFile::from(&settings);
            let text = toml::to_string(&file).unwrap();
            assert!(!text.contains("sk-speech-secret") && !text.contains("api_key"));
            let loaded: SpeechFile = toml::from_str(&text).unwrap();
            assert_eq!(loaded.to_draft(draft.api_key.clone()), draft);
        }
    }
}
