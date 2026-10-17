//! Data of the `/limits` card: the last quotas live sessions reported, and the choice between
//! a probe's answer and that cache. Pure: the hub owns the cache and passes the time.

use std::collections::HashMap;

use chrono::{DateTime, Utc};
use hub_core::domain::BackendKind;
use hub_core::questions::Button;
use hub_core::quota::{Label, QuotaReport, QuotaSource, QuotaUpdate, UsageWindow, WINDOW_LIMIT};

use crate::texts;

/// Callback data of the refresh button.
pub const REFRESH: &str = "limits:refresh";

/// One backend's probe: its report, or a failure the cache may stand in for.
pub type Outcome = (BackendKind, Result<QuotaReport, String>);

#[derive(Debug)]
struct Cached {
    plan: Option<Label>,
    windows: Vec<UsageWindow>,
    at: DateTime<Utc>,
}

/// One entry per backend with at most `WINDOW_LIMIT` windows.
#[derive(Debug, Default)]
pub struct QuotaCache {
    entries: HashMap<BackendKind, Cached>,
}

impl QuotaCache {
    /// A live session's rolling update, merged per window kind: a named window replaces the
    /// cached one of its kind, others keep their values, a missing plan keeps the cached plan.
    /// An update never clears data.
    pub fn apply(&mut self, update: QuotaUpdate, at: DateTime<Utc>) {
        if update.windows().is_empty() && update.plan.is_none() {
            return;
        }
        let backend = update.backend;
        let entry = self.entries.entry(backend).or_insert_with(|| Cached {
            plan: None,
            windows: Vec::new(),
            at,
        });
        for window in update.windows() {
            let same_kind = entry.windows.iter().position(|cached| cached.kind == window.kind);
            if let Some(slot) = same_kind.and_then(|index| entry.windows.get_mut(index)) {
                *slot = window.clone();
            } else if same_kind.is_none() && entry.windows.len() < WINDOW_LIMIT {
                entry.windows.push(window.clone());
            }
        }
        if update.plan.is_some() {
            entry.plan = update.plan;
        }
        entry.at = at;
    }

    /// What the card shows for one probe: a fresh report, which also replaces the cache; any
    /// other report is a final answer, returned as is; for a failure, the cache dated by its
    /// last update, or the failure's reason when there is none.
    pub fn settle(&mut self, outcome: Outcome, now: DateTime<Utc>) -> QuotaReport {
        let (backend, result) = outcome;
        match result {
            Ok(report) => {
                match &report.source {
                    QuotaSource::Fresh => {
                        let fresh = Cached {
                            plan: report.plan.clone(),
                            windows: report.windows().to_vec(),
                            at: now,
                        };
                        self.entries.insert(backend, fresh);
                    }
                    QuotaSource::Cached { .. }
                    | QuotaSource::NotReported
                    | QuotaSource::Unavailable(_) => {}
                }
                report
            }
            Err(reason) => match self.entries.get(&backend) {
                Some(Cached { plan, windows, at }) => {
                    QuotaReport::cached(backend, plan.clone(), windows.clone(), *at)
                }
                None => QuotaReport::unavailable(backend, &reason),
            },
        }
    }
}

/// The default backend first, the others in the order they came.
#[must_use]
pub fn ordered(reports: Vec<QuotaReport>, default: BackendKind) -> Vec<QuotaReport> {
    let (first, rest): (Vec<_>, Vec<_>) =
        reports.into_iter().partition(|report| report.backend == default);
    first.into_iter().chain(rest).collect()
}

#[must_use]
pub fn refresh_keyboard() -> Vec<Vec<Button>> {
    vec![vec![Button { text: texts::LIMITS_REFRESH.to_owned(), data: REFRESH.to_owned() }]]
}

#[cfg(test)]
mod tests {
    use hub_core::domain::BackendKind;
    use hub_core::quota::{
        Label, Percent, QuotaReport, QuotaSource, QuotaUpdate, UsageWindow, WindowKind,
    };

    use chrono::{DateTime, Utc};

    use super::*;

    fn at(raw: &str) -> DateTime<Utc> {
        raw.parse().unwrap()
    }

    fn label(raw: &str) -> Label {
        Label::new(raw).unwrap()
    }

    fn other(raw: &str) -> WindowKind {
        WindowKind::Other(label(raw))
    }

    fn window(kind: WindowKind, percent: i64) -> UsageWindow {
        UsageWindow { kind, percent: Percent::from_int(percent), resets_at: None }
    }

    fn update(backend: BackendKind, plan: Option<&str>, windows: Vec<UsageWindow>) -> QuotaUpdate {
        QuotaUpdate::new(backend, plan.map(label), windows)
    }

    fn cached(
        backend: BackendKind,
        plan: Option<&str>,
        windows: Vec<UsageWindow>,
        when: &str,
    ) -> QuotaReport {
        QuotaReport::cached(backend, plan.map(label), windows, at(when))
    }

    const T1: &str = "2026-10-16T10:00:00Z";
    const T2: &str = "2026-10-16T11:00:00Z";
    const T3: &str = "2026-10-16T12:00:00Z";

    #[test]
    fn rolling_updates_merge_by_window_and_keep_the_plan() {
        let mut cache = QuotaCache::default();
        cache.apply(
            update(BackendKind::Codex, Some("ChatGPT Plus"), vec![window(other("основное"), 10)]),
            at(T1),
        );
        cache.apply(update(BackendKind::Codex, None, vec![window(other("недельное"), 20)]), at(T2));
        cache.apply(update(BackendKind::Codex, None, vec![window(other("основное"), 30)]), at(T2));
        assert_eq!(
            cache.settle((BackendKind::Codex, Err("тайм-аут".to_owned())), at(T3)),
            cached(
                BackendKind::Codex,
                Some("ChatGPT Plus"),
                vec![window(other("основное"), 30), window(other("недельное"), 20)],
                T2
            )
        );
    }

    #[test]
    fn an_empty_update_never_clears_data() {
        let mut cache = QuotaCache::default();
        cache.apply(
            update(BackendKind::Claude, Some("Max"), vec![window(WindowKind::Week, 7)]),
            at(T1),
        );
        cache.apply(update(BackendKind::Claude, None, Vec::new()), at(T2));
        assert_eq!(
            cache.settle((BackendKind::Claude, Err("сбой".to_owned())), at(T3)),
            cached(BackendKind::Claude, Some("Max"), vec![window(WindowKind::Week, 7)], T1)
        );
    }

    #[test]
    fn an_empty_update_creates_no_entry() {
        let mut cache = QuotaCache::default();
        cache.apply(update(BackendKind::Claude, None, Vec::new()), at(T1));
        let report = cache.settle((BackendKind::Claude, Err("сбой".to_owned())), at(T2));
        assert!(matches!(report.source, QuotaSource::Unavailable(_)), "{report:?}");
    }

    #[test]
    fn a_fresh_report_replaces_the_cache_and_is_shown_as_is() {
        let mut cache = QuotaCache::default();
        cache.apply(
            update(BackendKind::Claude, None, vec![window(WindowKind::FiveHour, 10)]),
            at(T1),
        );
        let fresh = QuotaReport::fresh(
            BackendKind::Claude,
            Some(label("Max")),
            vec![window(WindowKind::Week, 5)],
        );
        assert_eq!(cache.settle((BackendKind::Claude, Ok(fresh.clone())), at(T2)), fresh);
        assert_eq!(
            cache.settle((BackendKind::Claude, Err("сбой".to_owned())), at(T3)),
            cached(BackendKind::Claude, Some("Max"), vec![window(WindowKind::Week, 5)], T2)
        );
    }

    #[test]
    fn a_definitive_answer_is_not_replaced_by_the_cache() {
        let mut cache = QuotaCache::default();
        cache.apply(update(BackendKind::Codex, None, vec![window(WindowKind::Week, 10)]), at(T1));
        let answer = QuotaReport::unavailable(
            BackendKind::Codex,
            "вход по API-ключу — лимитов подписки нет",
        );
        assert_eq!(cache.settle((BackendKind::Codex, Ok(answer.clone())), at(T2)), answer);
        let silent = QuotaReport::not_reported(BackendKind::Qwen);
        assert_eq!(cache.settle((BackendKind::Qwen, Ok(silent.clone())), at(T2)), silent);
        // the cache survived the definitive answer
        assert_eq!(
            cache.settle((BackendKind::Codex, Err("сбой".to_owned())), at(T3)),
            cached(BackendKind::Codex, None, vec![window(WindowKind::Week, 10)], T1)
        );
    }

    #[test]
    fn a_failure_without_a_cache_shows_its_reason() {
        let mut cache = QuotaCache::default();
        let reason = format!("сбой\n{}", "x".repeat(300));
        let report = cache.settle((BackendKind::Claude, Err(reason)), at(T1));
        let QuotaSource::Unavailable(shown) = &report.source else { panic!("{report:?}") };
        assert!(shown.as_str().starts_with("сбойxxx"), "{}", shown.as_str());
        assert_eq!(shown.as_str().chars().count(), hub_core::quota::REASON_LIMIT);
    }

    #[test]
    fn the_cache_is_bounded_per_backend() {
        let mut cache = QuotaCache::default();
        for batch in 0..3 {
            let many = (0..8).map(|i| window(other(&format!("w{batch}{i}")), 1)).collect();
            cache.apply(update(BackendKind::Claude, None, many), at(T1));
        }
        let report = cache.settle((BackendKind::Claude, Err("сбой".to_owned())), at(T2));
        assert_eq!(report.windows().len(), hub_core::quota::WINDOW_LIMIT);
    }

    #[test]
    fn the_default_backend_comes_first() {
        let reports = [BackendKind::Claude, BackendKind::Codex, BackendKind::Qwen]
            .into_iter()
            .map(QuotaReport::not_reported)
            .collect();
        let order: Vec<_> =
            ordered(reports, BackendKind::Codex).into_iter().map(|report| report.backend).collect();
        assert_eq!(order, [BackendKind::Codex, BackendKind::Claude, BackendKind::Qwen]);
    }

    #[test]
    fn the_refresh_button_fits_telegram() {
        assert_eq!(
            refresh_keyboard(),
            [[Button { text: "🔄 Обновить".to_owned(), data: REFRESH.to_owned() }]]
        );
        assert!(REFRESH.len() <= 64);
    }
}
