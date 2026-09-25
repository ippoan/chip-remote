//! tauri-build + Windows application manifest.
//!
//! tauri-build embeds its manifest (Common-Controls v6) only into the `[[bin]]`. The
//! lib's unit/integration test executables link the same Tauri code (comctl32
//! `TaskDialogIndirect`) and fail to start with STATUS_ENTRYPOINT_NOT_FOUND without it,
//! so the manifest is embedded into every linked artifact here instead (same approach as
//! tauri's own examples).

fn main() {
    let msvc = std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc")
        && std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows");
    let mut attrs = tauri_build::Attributes::new();
    if msvc {
        attrs =
            attrs.windows_attributes(tauri_build::WindowsAttributes::new_without_app_manifest());
    }
    tauri_build::try_build(attrs).expect("failed to run tauri-build");
    if msvc {
        let manifest = std::env::current_dir()
            .expect("cwd")
            .join("windows-app-manifest.xml");
        println!("cargo:rerun-if-changed={}", manifest.display());
        println!("cargo:rustc-link-arg=/MANIFEST:EMBED");
        println!("cargo:rustc-link-arg=/MANIFESTINPUT:{}", manifest.display());
    }
}
