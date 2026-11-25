//! Compile the app icon and version info into the .exe itself, so Explorer,
//! the taskbar and any shortcut pick it up without pointing at a loose .ico.

fn main() {
    println!("cargo:rerun-if-changed=assets/icon.ico");

    #[cfg(windows)]
    {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("assets/icon.ico");
        res.set("FileDescription", "FST — send a file straight off this PC");
        res.set("ProductName", "FST");
        if let Err(e) = res.compile() {
            // Not worth failing the build over: the app runs fine, it just
            // shows the default executable icon.
            println!("cargo:warning=could not embed icon: {e}");
        }
    }
}
