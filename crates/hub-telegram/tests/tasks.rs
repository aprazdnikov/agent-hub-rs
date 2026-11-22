use hub_telegram::hub::tasks::{Request, parse};
#[test]
fn task_commands_require_exact_run_revision_and_explicit_resume_step() {
    assert!(
        matches!(parse("create Проверка | Прочитать README"),Ok(Request::Create{title,prompt}) if title=="Проверка"&&prompt=="Прочитать README")
    );
    assert!(matches!(parse("run 1 2"), Ok(Request::Start { id: 1, revision: 2, next: None })));
    assert!(
        matches!(parse("resume 1 3 Проверить эффекты"),Ok(Request::Start{next:Some(step),..}) if step=="Проверить эффекты")
    );
    for invalid in
        ["run 1", "run 1 2 extra", "run x 2", "resume 1 2", "get 1 extra", "create no separator"]
    {
        assert!(parse(invalid).is_err(), "{invalid}");
    }
}

#[test]
fn workflow_commands_parse_status_archive_search_and_attempt() {
    use hub_core::tasks::TaskStatus;
    assert!(matches!(
        parse("status 1 3 accepted"),
        Ok(Request::Status { id: 1, revision: 3, status: TaskStatus::Accepted })
    ));
    assert!(matches!(
        parse("archive 1 4"),
        Ok(Request::Archive { id: 1, revision: 4, archived: true })
    ));
    assert!(matches!(parse("restore 1 5"), Ok(Request::Archive { archived: false, .. })));
    assert!(matches!(parse("attempt 1 2"), Ok(Request::Attempt { id: 1, run: 2 })));
    assert!(
        matches!(parse("list review Прочитать\nREADME"), Ok(Request::Search { query, .. }) if query == "Прочитать\nREADME")
    );
    for invalid in
        ["status 1 2 unknown", "archive 1", "restore 1 2 extra", "attempt 1 x", "attempt 1 2 extra"]
    {
        assert!(parse(invalid).is_err(), "{invalid}");
    }
}

#[test]
fn query_without_filter_searches_all_unarchived_statuses() {
    use hub_core::tasks::TaskFilter;
    for query in ["README", "Прочитать README", "Проверка\nистории"] {
        assert!(
            matches!(parse(&format!("list {query}")), Ok(Request::Search { filter: TaskFilter::Active, query: actual }) if actual == query)
        );
    }
}

#[test]
fn transfer_and_preview_commands_preserve_explicit_agent_and_step() {
    use hub_core::domain::BackendKind;
    assert!(matches!(
        parse("assign 2 5 qwen"),
        Ok(Request::Assign { id: 2, revision: 5, backend: BackendKind::Qwen })
    ));
    assert!(matches!(parse("preview 2"), Ok(Request::Preview { id: 2, next: None })));
    assert!(
        matches!(parse("preview 2 Check\n  code"), Ok(Request::Preview { id: 2, next: Some(step) }) if step == "Check\n  code")
    );
    for invalid in
        ["assign 2 5", "assign 2 5 unknown", "assign 2 5 qwen extra", "assign x 5 qwen", "preview"]
    {
        assert!(parse(invalid).is_err(), "{invalid}");
    }
}

#[test]
fn project_rules_commands_preserve_multiline_text_and_require_explicit_mode() {
    assert!(matches!(parse("rules"), Ok(Request::Rules)));
    assert!(matches!(parse("rules-set 4 on | Сначала Dev\n  затем ветка"),
        Ok(Request::SaveRules { revision: 4, enabled: true, text }) if text == "Сначала Dev\n  затем ветка"));
    assert!(matches!(
        parse("rules-set 5 off |"),
        Ok(Request::SaveRules { revision: 5, enabled: false, .. })
    ));
    assert!(parse("rules-set 5 yes | Rule").is_err());
}

#[test]
fn git_commands_preserve_exact_names_and_require_revisions() {
    assert!(
        matches!(parse("git 2 origin Dev"), Ok(Request::SaveGitPolicy { revision: 2, policy: Some(policy) }) if policy.base == "Dev")
    );
    assert!(matches!(parse("git 3 off"), Ok(Request::SaveGitPolicy { revision: 3, policy: None })));
    assert!(
        matches!(parse("prepare 4 5 feat/check"), Ok(Request::PrepareGit { id: 4, revision: 5, branch }) if branch == "feat/check")
    );
    assert!(parse("prepare 4 feat/check").is_err());
    assert!(matches!(parse("git-stop"), Ok(Request::CancelGit)));
}
