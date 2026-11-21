mod memory {
    mod cli;
    #[cfg(target_os = "linux")]
    mod publication;
}
