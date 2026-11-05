//! Windows targets: the icon and version info on the .exe, so Explorer shows the product
//! name. Decided by the target, not the build host, so a cross-build on Linux gets them too
//! (winresource then needs `llvm-rc`; `tools/build-windows.sh` finds it). Other targets: a no-op.

fn main() {
    println!("cargo:rerun-if-changed=data/icon.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let mut res = winresource::WindowsResource::new();
    res.set_icon("data/icon.ico")
        .set("ProductName", "Offline Road Maps")
        .set("FileDescription", "Offline Road Maps")
        .set("OriginalFilename", "OfflineRoadMaps.exe");
    if let Err(e) = res.compile() {
        println!("cargo:warning=no icon on the .exe: {e}");
    }
}
