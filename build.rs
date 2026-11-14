fn main() {
    println!("cargo::rerun-if-changed=packaging/windows/drukhal-manager.ico");
    // Checked at run time: build scripts are compiled for the host, the
    // .exe for the target.
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        windows_resources();
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
