//! Embeds the application icon into the Windows executable.

fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let mut resource = winresource::WindowsResource::new();
        resource.set_icon("../../assets/icons/nuntio.ico");
        resource
            .compile()
            .expect("failed to embed the Windows icon");
    }
    println!("cargo:rerun-if-changed=../../assets/icons/nuntio.ico");
}
