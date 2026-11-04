//! Locally configured, bounded connectivity checks. No network I/O in this module.

use std::collections::HashSet;
use std::net::IpAddr;

use serde::{Deserialize, Serialize};
use url::Url;

pub const MAX_CHECKS: usize = 8;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase", deny_unknown_fields)]
pub enum CheckDraft {
    Http { name: String, url: String, expected: u16 },
    Tcp { name: String, host: String, port: u16 },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    Http { url: Url, expected: u16 },
    Tcp { host: String, port: u16 },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Check {
    name: String,
    target: Target,
}

impl Check {
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }
    #[must_use]
    pub fn target(&self) -> &Target {
        &self.target
    }
    #[must_use]
    pub fn to_draft(&self) -> CheckDraft {
        match &self.target {
            Target::Http { url, expected } => CheckDraft::Http {
                name: self.name.clone(),
                url: url.to_string(),
                expected: *expected,
            },
            Target::Tcp { host, port } => {
                CheckDraft::Tcp { name: self.name.clone(), host: host.clone(), port: *port }
            }
        }
    }
}

pub fn parse(drafts: &[CheckDraft]) -> Result<Vec<Check>, String> {
    if drafts.len() > MAX_CHECKS {
        return Err(format!("Не более {MAX_CHECKS} проверок на проект"));
    }
    let mut names = HashSet::new();
    drafts
        .iter()
        .enumerate()
        .map(|(index, draft)| {
            let error = |reason: &str| format!("Проверка {}: {reason}", index + 1);
            let name = match draft {
                CheckDraft::Http { name, .. } | CheckDraft::Tcp { name, .. } => name.trim(),
            };
            if name.is_empty()
                || name.chars().count() > 80
                || name.chars().any(char::is_control)
                || !names.insert(name)
            {
                return Err(error("название: 1–80 символов, без повторов и переводов строк"));
            }
            let target = match draft {
                CheckDraft::Http { url, expected, .. } => {
                    if url.len() > 2048 || url.chars().any(char::is_control) {
                        return Err(error("URL: не более 2048 байт, без управляющих символов"));
                    }
                    let url = Url::parse(url.trim()).map_err(|_| error("некорректный URL"))?;
                    if !matches!(url.scheme(), "http" | "https")
                        || url.host_str().is_none()
                        || !url.username().is_empty()
                        || url.password().is_some()
                        || url.query().is_some()
                        || url.fragment().is_some()
                    {
                        return Err(error(
                            "нужен HTTP(S) URL без логина, пароля, query и fragment",
                        ));
                    }
                    if !(200..=599).contains(expected) {
                        return Err(error("ожидаемый HTTP-статус: 200–599"));
                    }
                    Target::Http { url, expected: *expected }
                }
                CheckDraft::Tcp { host, port, .. } => {
                    if *port == 0 {
                        return Err(error("TCP-порт: 1–65535"));
                    }
                    let host = host.trim();
                    let host = if let Ok(address) = host.parse::<IpAddr>() {
                        address.to_string()
                    } else {
                        if host.is_empty()
                            || host.len() > 253
                            || host.chars().any(|ch| {
                                ch.is_whitespace() || ch.is_control() || "/:@?#%\\".contains(ch)
                            })
                        {
                            return Err(error("укажите имя хоста или IP без схемы, пути и порта"));
                        }
                        url::Host::parse(host)
                            .map_err(|_| error("некорректное имя хоста"))?
                            .to_string()
                    };
                    Target::Tcp { host, port: *port }
                }
            };
            Ok(Check { name: name.to_owned(), target })
        })
        .collect()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Report {
    pub directory: Result<String, String>,
    pub cli: Result<String, String>,
    pub resources: Vec<(String, Result<String, String>)>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn http_and_tcp_checks_round_trip() {
        let drafts = vec![
            CheckDraft::Http {
                name: "Health".to_owned(),
                url: "https://internal.example/health".to_owned(),
                expected: 204,
            },
            CheckDraft::Tcp { name: "SSH".to_owned(), host: "::1".to_owned(), port: 22 },
        ];
        assert_eq!(parse(&drafts).unwrap().iter().map(Check::to_draft).collect::<Vec<_>>(), drafts);
        assert!(parse(&[]).unwrap().is_empty());
    }

    #[rstest::rstest]
    #[case("file:///etc/passwd")]
    #[case("ftp://example.org/file")]
    #[case("https://user:password@example.org/health")]
    #[case("https://example.org/health?token=secret")]
    #[case("https://example.org/health#fragment")]
    #[case("https://example.org/\nhealth")]
    fn unsafe_or_unsupported_http_urls_are_rejected(#[case] url: &str) {
        let draft =
            CheckDraft::Http { name: "Health".to_owned(), url: url.to_owned(), expected: 200 };
        assert!(parse(&[draft]).is_err());
    }

    #[test]
    fn names_limits_status_codes_and_tcp_addresses_are_validated() {
        for draft in [
            CheckDraft::Tcp { name: String::new(), host: "localhost".to_owned(), port: 80 },
            CheckDraft::Tcp {
                name: "tcp".to_owned(),
                host: "https://host/path".to_owned(),
                port: 80,
            },
            CheckDraft::Tcp { name: "tcp".to_owned(), host: "host".to_owned(), port: 0 },
            CheckDraft::Http {
                name: "http".to_owned(),
                url: "https://host/".to_owned(),
                expected: 999,
            },
        ] {
            assert!(parse(&[draft]).is_err());
        }
        let draft =
            CheckDraft::Tcp { name: "tcp".to_owned(), host: "localhost".to_owned(), port: 80 };
        assert!(parse(&[draft.clone(), draft.clone()]).is_err());
        assert!(parse(&vec![draft; 9]).is_err());
    }
}
