# Privacy policy

Last updated: 28 September 2026

## nuntio

nuntio doesn't collect, store or send any personal data. It has no telemetry, no update check and no crash reporting, and it doesn't contact any server of its own.

This program will not transfer any information to other networked systems unless specifically requested by the user or the person installing or operating it.

nuntio only goes online when you ask it to: when you click a link, it hands the address to your browser. The programs you run inside nuntio, such as `ssh` or `curl`, do their own networking.

nuntio keeps these files on your computer, and they never leave it:

- your configuration file
- a log file, only when nuntio is started without a terminal (for example `~/.cache/nuntio/nuntio.log` on Linux or `~/Library/Caches/nuntio/nuntio.log` on macOS)

nuntio uses the clipboard when you copy or paste, and when a program copies text with OSC 52 (the `clipboard_write` setting, which you can turn off). Programs running in nuntio can never read the clipboard.

## Website and downloads

The website at [cebor.github.io/nuntio](https://cebor.github.io/nuntio/) is a set of static pages. It sets no cookies, uses no analytics or tracking, and loads nothing from third parties.

The website and the downloads are hosted by GitHub (GitHub Pages and GitHub Releases). GitHub processes technical data such as your IP address to serve them; see the [GitHub General Privacy Statement](https://docs.github.com/en/site-policy/privacy-policies/github-general-privacy-statement).

## Contact

Felix Itzenplitz. Questions about this policy go to the [issue tracker](https://github.com/cebor/nuntio/issues) on GitHub.
