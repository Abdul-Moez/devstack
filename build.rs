// Embeds the icon and version info into the Windows executables.
fn main() {
    println!("cargo:rerun-if-changed=assets/devstack.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("assets/devstack.ico");
        res.set("ProductName", "devstack");
        res.set("FileDescription", "devstack local development environment");
        if let Err(e) = res.compile() {
            println!("cargo:warning=could not embed the icon: {e}");
        }
    }
}
