#[cfg(test)]
mod tests {
    use hub_core::tasks::git::{GitPolicy, Preparation, PreparationState};
    use hub_telegram::git::Request;
    use std::{path::Path, process::Command};
    use tokio_util::sync::CancellationToken;

    fn git(path: &Path, args: &[&str]) -> String {
        let output = Command::new("git").args(args).current_dir(path).output().unwrap();
        assert!(
            output.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap().trim().into()
    }
    struct Repo {
        root: tempfile::TempDir,
        seed: std::path::PathBuf,
        work: std::path::PathBuf,
    }
    impl Repo {
        fn new() -> Self {
            let root = tempfile::tempdir().unwrap();
            let remote = root.path().join("remote.git");
            let seed = root.path().join("seed");
            let work = root.path().join("work");
            git(root.path(), &["init", "--bare", "--initial-branch=Dev", remote.to_str().unwrap()]);
            git(root.path(), &["clone", remote.to_str().unwrap(), seed.to_str().unwrap()]);
            git(&seed, &["config", "user.name", "Test"]);
            git(&seed, &["config", "user.email", "test@example.invalid"]);
            std::fs::write(seed.join("README"), "initial").unwrap();
            git(&seed, &["add", "."]);
            git(&seed, &["commit", "-m", "initial"]);
            git(&seed, &["push", "origin", "Dev"]);
            git(root.path(), &["clone", remote.to_str().unwrap(), work.to_str().unwrap()]);
            git(&work, &["config", "user.name", "Test"]);
            git(&work, &["config", "user.email", "test@example.invalid"]);
            Self { root, seed, work }
        }
        fn request(&self) -> Request {
            Request {
                project: self.work.clone(),
                preparation: Preparation {
                    policy: GitPolicy { remote: "origin".into(), base: "Dev".into() },
                    branch: "feat/task".into(),
                    state: PreparationState::Preparing,
                    sha: String::new(),
                    message: String::new(),
                },
                verify_only: false,
                continuation: false,
            }
        }
        fn advance(&self) -> String {
            std::fs::write(self.seed.join("README"), "updated").unwrap();
            git(&self.seed, &["commit", "-am", "update"]);
            git(&self.seed, &["push", "origin", "Dev"]);
            git(&self.seed, &["rev-parse", "HEAD"])
        }
    }
    #[tokio::test]
    async fn preparation_fetches_exact_base_and_launch_checks_branch_and_dirty_tree() {
        let repo = Repo::new();
        let expected = repo.advance();
        let mut request = repo.request();
        let sha = hub_app::git::run(request.clone(), CancellationToken::new()).await.unwrap();
        assert_eq!(sha, expected);
        assert_eq!(git(&repo.work, &["branch", "--show-current"]), "feat/task");
        assert_eq!(git(&repo.work, &["rev-parse", "Dev"]), expected);
        request.preparation.sha = sha;
        request.preparation.state = PreparationState::Ready;
        request.verify_only = true;
        assert!(hub_app::git::run(request.clone(), CancellationToken::new()).await.is_ok());
        std::fs::write(repo.work.join("draft"), "agent result").unwrap();
        assert!(hub_app::git::run(request.clone(), CancellationToken::new()).await.is_err());
        request.continuation = true;
        assert!(hub_app::git::run(request.clone(), CancellationToken::new()).await.is_ok());
        git(&repo.work, &["switch", "Dev"]);
        assert!(hub_app::git::run(request, CancellationToken::new()).await.is_err());
    }
    #[tokio::test]
    async fn dirty_existing_branch_wrong_case_and_ahead_base_are_refused() {
        let repo = Repo::new();
        let original = git(&repo.work, &["rev-parse", "Dev"]);
        std::fs::write(repo.work.join("draft"), "keep").unwrap();
        assert!(
            hub_app::git::run(repo.request(), CancellationToken::new())
                .await
                .unwrap_err()
                .contains("незакоммиченные")
        );
        assert_eq!(std::fs::read_to_string(repo.work.join("draft")).unwrap(), "keep");
        std::fs::remove_file(repo.work.join("draft")).unwrap();
        git(&repo.work, &["branch", "feat/task"]);
        assert!(
            hub_app::git::run(repo.request(), CancellationToken::new())
                .await
                .unwrap_err()
                .contains("существует")
        );
        let mut request = repo.request();
        request.preparation.branch = "feat/new".into();
        request.preparation.policy.base = "dev".into();
        assert!(hub_app::git::run(request.clone(), CancellationToken::new()).await.is_err());
        assert_eq!(git(&repo.work, &["rev-parse", "Dev"]), original);
        request.preparation.policy.base = "Dev".into();
        git(&repo.work, &["commit", "--allow-empty", "-m", "local only"]);
        let ahead = git(&repo.work, &["rev-parse", "HEAD"]);
        assert!(
            hub_app::git::run(request, CancellationToken::new())
                .await
                .unwrap_err()
                .contains("отсутствующие")
        );
        assert_eq!(git(&repo.work, &["rev-parse", "Dev"]), ahead);
    }
    #[tokio::test]
    async fn worktree_occupancy_and_cancellation_are_refused_without_branch_creation() {
        let repo = Repo::new();
        git(&repo.work, &["switch", "-c", "other"]);
        git(
            &repo.work,
            &["worktree", "add", repo.root.path().join("occupied").to_str().unwrap(), "Dev"],
        );
        assert!(
            hub_app::git::run(repo.request(), CancellationToken::new())
                .await
                .unwrap_err()
                .contains("другом worktree")
        );
        let cancel = CancellationToken::new();
        cancel.cancel();
        assert!(hub_app::git::run(repo.request(), cancel).await.is_err());
        assert_eq!(git(&repo.work, &["branch", "--show-current"]), "other");
    }

    #[tokio::test]
    async fn preparation_preserves_ignored_files_that_collide_with_remote_update() {
        let repo = Repo::new();
        std::fs::write(repo.work.join(".git/info/exclude"), "private-cache\n").unwrap();
        std::fs::write(repo.work.join("private-cache"), "local private data").unwrap();
        std::fs::write(repo.seed.join("private-cache"), "remote content").unwrap();
        git(&repo.seed, &["add", "private-cache"]);
        git(&repo.seed, &["commit", "-m", "add file"]);
        git(&repo.seed, &["push", "origin", "Dev"]);
        assert!(hub_app::git::run(repo.request(), CancellationToken::new()).await.is_err());
        assert_eq!(
            std::fs::read_to_string(repo.work.join("private-cache")).unwrap(),
            "local private data"
        );
        assert_eq!(git(&repo.work, &["branch", "--show-current"]), "Dev");
    }

    #[tokio::test]
    async fn branch_tag_name_collision_never_advances_checked_out_base_behind_index() {
        let repo = Repo::new();
        git(&repo.work, &["tag", "Dev"]);
        git(&repo.work, &["tag", "feat/task"]);
        let expected = repo.advance();
        let mut request = repo.request();
        assert_eq!(
            hub_app::git::run(request.clone(), CancellationToken::new()).await.unwrap(),
            expected
        );
        assert_eq!(std::fs::read_to_string(repo.work.join("README")).unwrap(), "updated");
        assert_eq!(git(&repo.work, &["status", "--porcelain"]), "");
        request.verify_only = true;
        request.preparation.state = PreparationState::Ready;
        request.preparation.sha = expected;
        assert!(hub_app::git::run(request, CancellationToken::new()).await.is_ok());
    }
    #[cfg(unix)]
    #[tokio::test]
    async fn detached_rebase_worktree_reserves_its_original_branch() {
        let repo = Repo::new();
        repo.advance();
        git(&repo.work, &["pull", "--ff-only"]);
        git(&repo.work, &["switch", "-c", "other"]);
        let occupied = repo.root.path().join("occupied");
        git(&repo.work, &["worktree", "add", occupied.to_str().unwrap(), "Dev"]);
        git(
            &occupied,
            &["-c", "sequence.editor=sed -i 's/^pick /edit /'", "rebase", "-i", "HEAD~1"],
        );
        assert!(git(&repo.work, &["worktree", "list", "--porcelain"]).contains("detached"));
        assert!(hub_app::git::run(repo.request(), CancellationToken::new()).await.is_err());
        assert_eq!(git(&repo.work, &["branch", "--show-current"]), "other");
    }
    #[tokio::test]
    async fn explicit_fetch_does_not_update_ambient_remote_mappings() {
        let repo = Repo::new();
        git(
            &repo.work,
            &["config", "--add", "remote.origin.fetch", "+refs/heads/*:refs/remotes/extra/*"],
        );
        repo.advance();
        hub_app::git::run(repo.request(), CancellationToken::new()).await.unwrap();
        let extra = Command::new("git")
            .args(["show-ref", "--verify", "refs/remotes/extra/Dev"])
            .current_dir(&repo.work)
            .output()
            .unwrap();
        assert!(
            !extra.status.success(),
            "fetch must only update the explicitly chosen tracking ref"
        );
    }
}
