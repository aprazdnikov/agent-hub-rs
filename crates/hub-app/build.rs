//! Embeds the application icon into the Windows executable.

fn main() {
    println!("cargo:rerun-if-changed=../../assets/agent-hub.ico");
    #[cfg(windows)]
    {
        let mut resource = winresource::WindowsResource::new();
        resource.set_icon("../../assets/agent-hub.ico");
        if let Err(error) = resource.compile() {
            // A missing resource compiler must fail the build loudly, not ship without an icon.
            println!("cargo:warning=icon not embedded: {error}");
            std::process::exit(1);
        }
    }
}
