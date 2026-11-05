//! Windows: the icon and version info on the .exe, so Explorer shows the product name.
//! Only when building on Windows (the build dependency is Windows-only); elsewhere a no-op.

fn main() {
    println!("cargo:rerun-if-changed=data/icon.ico");
    #[cfg(windows)]
    {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("data/icon.ico")
            .set("ProductName", "Offline Road Maps")
            .set("FileDescription", "Offline Road Maps")
            .set("OriginalFilename", "OfflineRoadMaps.exe");
        if let Err(e) = res.compile() {
            println!("cargo:warning=no icon on the .exe: {e}");
        }
    }
}
