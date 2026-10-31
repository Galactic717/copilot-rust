fn main() {
    println!("cargo:rerun-if-changed=resources/icon.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("resources/icon.ico")
            .set("ProductName", "Copilot")
            .set("FileDescription", "Copilot")
            .set("CompanyName", "Copilot");
        if let Err(e) = res.compile() {
            // A missing Windows SDK (rc.exe) only costs the exe icon; the app itself still builds.
            println!("cargo:warning=icon resource not embedded: {e}");
        }
    }
}
