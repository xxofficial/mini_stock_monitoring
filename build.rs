use std::env;

fn main() {
    println!("cargo:rerun-if-changed=assets/mini-stock-monitor.ico");

    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }

    let version = env::var("CARGO_PKG_VERSION").expect("Cargo provides the package version");
    let mut resource = winres::WindowsResource::new();
    resource
        .set_icon("assets/mini-stock-monitor.ico")
        .set("FileDescription", "微行情")
        .set("ProductName", "微行情")
        .set("ProductVersion", &version)
        .set("FileVersion", &version)
        .set("OriginalFilename", "微行情.exe")
        .set("InternalName", "mini-stock-monitor")
        .set("LegalCopyright", "Copyright © 2026");
    resource.compile().expect("failed to compile Windows resources");
}
