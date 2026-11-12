#[path = "src/sidecar.rs"]
mod sidecar;

fn main() {
    let manifest = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let target = std::env::var("TARGET").expect("Cargo supplies TARGET to build scripts");
    let vendor = manifest.join("../../../vendor/tailpocket/bin");
    let source = vendor.join(sidecar::vendor_name(&target).expect("supported desktop target"));
    let output = manifest
        .join("binaries")
        .join(sidecar::bundled_name(&target));
    println!("cargo:rerun-if-changed={}", source.display());
    println!("cargo:rerun-if-changed=src/sidecar.rs");
    std::fs::create_dir_all(manifest.join("binaries")).expect("create sidecar staging directory");
    std::fs::copy(&source, &output).unwrap_or_else(|error| {
        panic!(
            "stage the predownloaded TailPocket helper {}: {error}",
            source.display()
        )
    });
    if target.ends_with("-windows-msvc") {
        // tauri-build embeds resources for the app binary, but Rust unit-test
        // harnesses also need Common Controls v6 when using Tauri's MockRuntime.
        // Otherwise the loader cannot resolve comctl32!TaskDialogIndirect.
        println!("cargo:rustc-link-arg=/MANIFEST:EMBED");
        println!(
            "cargo:rustc-link-arg=/MANIFESTDEPENDENCY:type='win32' name='Microsoft.Windows.Common-Controls' version='6.0.0.0' processorArchitecture='*' publicKeyToken='6595b64144ccf1df' language='*'"
        );
        // Use the linker for every executable, rather than also emitting a
        // binary-only RT_MANIFEST resource, which would duplicate the manifest.
        tauri_build::try_build(
            tauri_build::Attributes::new()
                .windows_attributes(tauri_build::WindowsAttributes::new_without_app_manifest()),
        )
        .expect("build Tauri resources");
    } else {
        tauri_build::build();
    }
}
