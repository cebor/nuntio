#[cfg(target_os = "linux")]
fn main() {
    std::process::exit(nuntio_wsl::relay::run(
        std::env::args_os().skip(1).collect(),
    ));
}

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("nuntio-wsl runs inside WSL, started by nuntio on Windows");
    std::process::exit(2);
}
