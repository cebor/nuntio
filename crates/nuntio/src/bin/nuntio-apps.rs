//! `nuntio-apps`, terminal apps that nuntio opens in its panes (`nuntio-apps
//! notes`). A console program on every platform, shipped next to nuntio but
//! only put on the PATH inside nuntio's panes.

fn main() -> anyhow::Result<()> {
    nuntio_apps::main()
}
