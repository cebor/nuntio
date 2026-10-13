#[cfg(target_os = "linux")]
fn main() {
    let mut args = std::env::args_os().skip(1);
    // `socket <path>` bridges stdin/stdout to a Unix socket; anything else is the PTY relay.
    let code = match (args.next(), args.next(), args.next()) {
        (Some(mode), Some(path), None) if mode == "socket" => {
            nuntio_wsl::relay::socket_bridge(path)
        }
        _ => nuntio_wsl::relay::run(std::env::args_os().skip(1).collect()),
    };
    std::process::exit(code);
}

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("nuntio-wsl runs inside WSL, started by nuntio on Windows");
    std::process::exit(2);
}
