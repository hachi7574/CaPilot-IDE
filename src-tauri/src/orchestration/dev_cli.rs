//! Development-only installation of the Rust `capilot` CLI.
//!
//! `pnpm tauri dev` builds `target/debug/capilot` before Tauri starts. Agent
//! processes already receive `~/CaPilot/bin` at the front of PATH, so a stable
//! symlink there makes the CLI available without a Python or shell shim.

use std::path::PathBuf;

#[cfg(all(debug_assertions, unix))]
pub fn install_dev_cli() -> std::io::Result<Option<PathBuf>> {
    let current_exe = std::env::current_exe()?;
    let target_dir = current_exe
        .parent()
        .ok_or_else(|| std::io::Error::other("Tauri executable has no parent directory"))?;
    let rust_cli = target_dir.join("capilot");
    if !rust_cli.is_file() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!(
                "Rust capilot CLI not found at {}; run through `pnpm tauri dev`",
                rust_cli.display()
            ),
        ));
    }

    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or_else(|| std::io::Error::other("HOME is not set"))?;
    install_dev_cli_from(&rust_cli, &home).map(Some)
}

#[cfg(all(debug_assertions, unix))]
fn install_dev_cli_from(
    rust_cli: &std::path::Path,
    home: &std::path::Path,
) -> std::io::Result<PathBuf> {
    use std::os::unix::fs::symlink;

    let bin_dir = home.join("CaPilot").join("bin");
    std::fs::create_dir_all(&bin_dir)?;
    let link = bin_dir.join("capilot");

    if std::fs::read_link(&link).ok().as_deref() == Some(rust_cli) {
        return Ok(link);
    }
    match std::fs::symlink_metadata(&link) {
        Ok(metadata) if metadata.file_type().is_symlink() || metadata.is_file() => {
            std::fs::remove_file(&link)?;
        }
        Ok(_) => {
            return Err(std::io::Error::other(format!(
                "development CLI path is not a file: {}",
                link.display()
            )));
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    symlink(&rust_cli, &link)?;
    Ok(link)
}

#[cfg(not(all(debug_assertions, unix)))]
pub fn install_dev_cli() -> std::io::Result<Option<PathBuf>> {
    Ok(None)
}

#[cfg(all(test, debug_assertions, unix))]
mod tests {
    #[test]
    fn development_install_replaces_old_script_with_rust_binary_symlink() {
        let root = std::env::temp_dir().join(format!(
            "capilot-dev-link-{}",
            uuid::Uuid::new_v4().simple()
        ));
        let target_dir = root.join("target").join("debug");
        std::fs::create_dir_all(&target_dir).unwrap();
        let rust_cli = target_dir.join("capilot");
        std::fs::write(&rust_cli, b"rust binary").unwrap();
        let bin_dir = root.join("home").join("CaPilot").join("bin");
        std::fs::create_dir_all(&bin_dir).unwrap();
        let old_script = bin_dir.join("capilot");
        std::fs::write(&old_script, b"#!/bin/false").unwrap();

        let installed = super::install_dev_cli_from(&rust_cli, &root.join("home")).unwrap();
        assert_eq!(installed, old_script);
        assert_eq!(std::fs::read_link(&installed).unwrap(), rust_cli);
        assert_eq!(std::fs::read(&installed).unwrap(), b"rust binary");
        let _ = std::fs::remove_dir_all(root);
    }
}
