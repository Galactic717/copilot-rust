fn main() {
    println!("cargo:rerun-if-changed=resources/icon.ico");
    println!("cargo:rerun-if-changed=locales");
    println!("cargo:rerun-if-env-changed=COPILOT_RELEASE_BUILD");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        embed_vc_runtime();
        let version = env!("CARGO_PKG_VERSION");
        let mut res = winresource::WindowsResource::new();
        res.set_icon("resources/icon.ico")
            .set("ProductName", "Copilot")
            .set("FileDescription", "Copilot")
            .set("CompanyName", "Copilot")
            .set("ProductVersion", version)
            .set("FileVersion", version)
            .set("LegalCopyright", "MIT License. See LICENSE.");
        if let Err(e) = res.compile() {
            // A missing Windows SDK (rc.exe) only costs the exe icon; the app itself still builds.
            println!("cargo:warning=icon resource not embedded: {e}");
        }
    }
}

/// Copies the VC++ runtime DLLs into OUT_DIR and generates the table `proc::ensure_vc_runtime` writes out next to the sidecars.
fn embed_vc_runtime() {
    let out = std::path::PathBuf::from(std::env::var("OUT_DIR").unwrap());
    let sys = std::path::PathBuf::from(std::env::var("WINDIR").unwrap_or_else(|_| "C:\\Windows".into())).join("System32");
    let mut table = String::from("pub static VCRT: &[(&str, &[u8])] = &[\n");
    for name in ["msvcp140.dll", "vcruntime140.dll", "vcruntime140_1.dll", "vcomp140.dll"] {
        let dest = out.join(name);
        if std::fs::copy(sys.join(name), &dest).is_ok() {
            table += &format!("    ({name:?}, include_bytes!({:?})),\n", dest.display().to_string());
        } else {
            // A release without it would look fine here and fail on a clean Windows.
            assert!(std::env::var_os("COPILOT_RELEASE_BUILD").is_none(), "{name} is missing from System32");
            println!("cargo:warning={name} not found; sidecars will need the VC++ redistributable");
        }
    }
    table += "];\n";
    std::fs::write(out.join("vcrt.rs"), table).unwrap();
}
