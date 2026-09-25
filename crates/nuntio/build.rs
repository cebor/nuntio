//! Embeds the application icon and file properties into the Windows
//! executable.

fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let mut resource = winresource::WindowsResource::new();
        resource.set_icon("../../assets/icons/nuntio.ico");
        resource.set("ProductName", "nuntio");
        // Task Manager shows this as the process name.
        resource.set("FileDescription", "nuntio");
        resource.set(
            "LegalCopyright",
            "© 2026 Felix Itzenplitz. MIT or Apache-2.0.",
        );
        resource
            .compile()
            .expect("failed to embed the Windows resources");
    }
    println!("cargo:rerun-if-changed=../../assets/icons/nuntio.ico");
}
