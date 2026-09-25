//! `nuntio-config`, the config editor. A console program on every platform
//! (unlike `nuntio` itself on Windows), shipped next to nuntio but only put
//! on the PATH inside nuntio's panes.

fn main() -> anyhow::Result<()> {
    nuntio_config_tui::main()
}
