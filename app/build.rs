fn main() {
    // The dimple version the flight model names on its flight results.
    println!("cargo:rerun-if-changed=../Cargo.lock");
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=assets/icon.ico");
    let lock = std::fs::read_to_string("../Cargo.lock").unwrap_or_default();
    let dimple = lock
        .split("[[package]]")
        .find(|p| p.contains("name = \"dimple\""))
        .and_then(|p| p.lines().find_map(|l| l.strip_prefix("version = \"")))
        .and_then(|v| v.strip_suffix('"'))
        .unwrap_or("unknown");
    println!("cargo:rustc-env=DIMPLE_VERSION={dimple}");
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
