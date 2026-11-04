#[cfg(target_os = "linux")]
#[test]
fn login_duplicate_exits_without_a_window_or_connecting_the_bot() {
    use hub_app::instance::InstanceLock;
    use std::{
        process::{Command, Stdio},
        time::{Duration, Instant},
    };
    let temp = tempfile::tempdir().unwrap();
    let data = temp.path().join("data");
    let _lock = InstanceLock::acquire(&data.join("agent-hub/agent-hub.lock")).unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_agent-hub"))
        .arg("--autostart")
        .env("XDG_DATA_HOME", &data)
        .env("XDG_CONFIG_HOME", temp.path().join("config"))
        .env_remove("DISPLAY")
        .env_remove("WAYLAND_DISPLAY")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(3);
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if Instant::now() >= deadline {
            child.kill().unwrap();
            child.wait().unwrap();
            panic!("duplicate login launch did not exit");
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    assert!(status.success());
    assert!(!temp.path().join("config").exists());
    assert!(!data.join("agent-hub/logs").exists());
}
