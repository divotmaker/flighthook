fn main() {
    // Embed the .ico as a Windows resource so Explorer/taskbar show our icon.
    // CARGO_CFG_TARGET_OS is the *target* OS (not host), correct for cross-compile.
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("assets/icon.ico");
        // Windows shows FileDescription in Task Manager, the taskbar tooltip
        // and "Open with"; winresource would otherwise use the package name.
        res.set("FileDescription", "flighthook");
        res.set("ProductName", "flighthook");
        res.set("InternalName", "flighthook.exe");
        res.set("OriginalFilename", "flighthook.exe");
        res.compile().unwrap();
    }
}
