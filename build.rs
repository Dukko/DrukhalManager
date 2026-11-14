fn main() {
    println!("cargo::rerun-if-changed=packaging/windows/drukhal-manager.ico");
    println!("cargo::rerun-if-changed=po");
    println!("cargo::rerun-if-changed=build.rs");
    compile_catalogs();
    // Checked at run time: build scripts are compiled for the host, the
    // .exe for the target.
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        windows_resources();
    }
}

/// Compiles `po/*.po` into the `.mo` catalogs gettext reads, next to the
/// build they belong to, in `target/i18n/locale`.
///
/// The package builds (Flatpak, the AUR PKGBUILD, packaging/windows/bundle.sh)
/// copy that folder into `share/locale`. Keeping the compilation here means
/// `cargo run` and `cargo test` see the translations too, and a `.po` with a
/// mistake fails the build instead of producing a broken catalog.
fn compile_catalogs() {
    use std::path::Path;
    use std::process::Command;

    let out = Path::new("target/i18n/locale");
    let mut catalogs: Vec<_> = std::fs::read_dir("po")
        .into_iter()
        .flatten()
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|extension| extension == "po"))
        .collect();
    catalogs.sort();
    // A language removed from po/ shouldn't keep its old .mo around.
    if out.is_dir() {
        std::fs::remove_dir_all(out).expect("could not clear target/i18n/locale");
    }
    for catalog in catalogs {
        let language = catalog.file_stem().expect("a .po file has a name");
        let mo = out.join(language).join("LC_MESSAGES").join("drukhal-manager.mo");
        std::fs::create_dir_all(mo.parent().expect("the .mo has a folder")).expect("could not make the catalog folder");
        let status = Command::new("msgfmt")
            .args(["--check", "--output-file"])
            .arg(&mo)
            .arg(&catalog)
            .status()
            .unwrap_or_else(|e| panic!("could not run msgfmt from gettext, needed for {}: {e}", catalog.display()));
        assert!(status.success(), "msgfmt rejected {}", catalog.display());
    }
}

#[cfg(windows)]
fn windows_resources() {
    let mut res = winresource::WindowsResource::new();
    res.set_icon("packaging/windows/drukhal-manager.ico");
    res.set("ProductName", "DrukhalManager");
    res.set("FileDescription", "DrukhalManager");
    res.compile().expect("could not embed the Windows resources");
}

#[cfg(not(windows))]
fn windows_resources() {}
