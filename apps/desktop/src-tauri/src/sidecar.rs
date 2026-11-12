//! Shared by build.rs and its unit tests. Upstream release names are not Tauri triples.

pub fn vendor_name(target: &str) -> Result<String, String> {
    let arch = if target.starts_with("x86_64-") {
        "amd64"
    } else if target.starts_with("aarch64-") {
        "arm64"
    } else {
        return Err(format!("unsupported TailPocket architecture: {target}"));
    };
    let (os, extension) = if target.contains("-windows-") {
        ("windows", ".exe")
    } else if target.ends_with("-apple-darwin") {
        ("darwin", "")
    } else if target.contains("-linux-") {
        ("linux", "")
    } else {
        return Err(format!("unsupported TailPocket platform: {target}"));
    };
    Ok(format!("tailpocket-host-{os}-{arch}{extension}"))
}

pub fn bundled_name(target: &str) -> String {
    let extension = if target.contains("-windows-") {
        ".exe"
    } else {
        ""
    };
    format!("tailpocket-host-{target}{extension}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn release_names_and_tauri_names_are_distinct() {
        for (target, release) in [
            ("x86_64-pc-windows-msvc", "windows-amd64.exe"),
            ("aarch64-pc-windows-msvc", "windows-arm64.exe"),
            ("x86_64-apple-darwin", "darwin-amd64"),
            ("aarch64-apple-darwin", "darwin-arm64"),
            ("x86_64-unknown-linux-gnu", "linux-amd64"),
            ("aarch64-unknown-linux-gnu", "linux-arm64"),
        ] {
            assert_eq!(
                vendor_name(target).unwrap(),
                format!("tailpocket-host-{release}")
            );
            assert!(bundled_name(target).contains(target));
        }
        assert!(vendor_name("i686-pc-windows-msvc").is_err());
        assert!(vendor_name("aarch64-apple-ios").is_err());
    }

    #[test]
    fn config_and_staged_binary_match() {
        let config: serde_json::Value =
            serde_json::from_str(include_str!("../tauri.conf.json")).unwrap();
        assert_eq!(
            config["bundle"]["externalBin"][0],
            "binaries/tailpocket-host"
        );
        let target = env!("TAURI_ENV_TARGET_TRIPLE");
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let source = root
            .join("../../../vendor/tailpocket/bin")
            .join(vendor_name(target).unwrap());
        let staged = root.join("binaries").join(bundled_name(target));
        assert_eq!(
            std::fs::read(source).unwrap(),
            std::fs::read(staged).unwrap()
        );
    }
}
