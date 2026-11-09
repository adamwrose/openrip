// Embeds the app icon in openrip.exe so Explorer, the taskbar and shortcuts show it.
fn main() {
    println!("cargo:rerun-if-changed=packaging/windows/openrip.ico");
    #[cfg(windows)]
    if std::env::var("CARGO_CFG_TARGET_OS").is_ok_and(|os| os == "windows") {
        winresource::WindowsResource::new()
            .set_icon("packaging/windows/openrip.ico")
            .compile()
            .expect("couldn't embed the Windows icon");
    }
}
