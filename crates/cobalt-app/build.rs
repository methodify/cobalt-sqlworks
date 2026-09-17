fn main() {
    // Embed the application icon (and version info) into the Windows executable so Explorer,
    // the taskbar and the installer show the Cobalt mark instead of the default icon.
    #[cfg(windows)]
    {
        println!("cargo:rerun-if-changed=../../assets/icon.ico");
        let mut res = winresource::WindowsResource::new();
        res.set_icon("../../assets/icon.ico");
        res.set("ProductName", "Cobalt SQL Works");
        res.set("FileDescription", "Cobalt SQL Works");
        res.set("LegalCopyright", "MIT OR Apache-2.0");
        if let Err(e) = res.compile() {
            println!("cargo:warning=could not embed the Windows icon: {e}");
        }
    }
}
