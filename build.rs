fn main() {
    let config = slint_build::CompilerConfiguration::new().with_style("fluent-dark".into());
    slint_build::compile_with_config("ui/app.slint", config)
        .expect("failed to compile the Slint UI");

    // Patina.app carries libmpv and the libraries it needs in
    // Contents/Frameworks, and they name each other by @rpath without an
    // rpath of their own, so the executable supplies it. (The target's OS,
    // not this build script's.)
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
        println!("cargo:rustc-link-arg-bins=-Wl,-rpath,@executable_path/../Frameworks");
    }

    windows_resources();
}

/// The icon and the version details inside patina.exe. A Windows executable
/// has nothing beside it to carry an icon, unlike a macOS bundle or a Linux
/// desktop entry, and without one Explorer, the taskbar and Alt-Tab show
/// the generic one.
#[cfg(windows)]
fn windows_resources() {
    println!("cargo:rerun-if-changed=packaging/icons/patina.ico");
    let mut resources = winresource::WindowsResource::new();
    resources.set_icon("packaging/icons/patina.ico");
    // FileDescription is the name Windows shows: in the firewall prompt and
    // Task Manager. The sentence goes in Comments.
    resources.set("FileDescription", "Patina");
    resources.set("ProductName", "Patina");
    resources.set("Comments", env!("CARGO_PKG_DESCRIPTION"));
    resources.set("LegalCopyright", "MIT licensed");
    // An icon is not worth failing a build over.
    if let Err(error) = resources.compile() {
        println!("cargo:warning=no icon embedded: {error}");
    }
}

#[cfg(not(windows))]
fn windows_resources() {}
