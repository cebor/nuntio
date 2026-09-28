# Privacy policy

Last updated: 28 September 2026

## nuntio

nuntio doesn't collect, store or send any personal data. It has no telemetry and no crash reporting, and it doesn't contact any server of its own.

This program will not transfer any information to other networked systems unless specifically requested by the user or the person installing or operating it.

nuntio only goes online when you ask it to:

- When you click a link, it hands the address to your browser.
- When you turn on the update check (`updates.check`, off by default) or run the `check_for_updates` action, nuntio asks GitHub's API (`api.github.com`) for the latest nuntio release, at most once a day for the setting. The request carries nuntio's version in its `User-Agent` header and nothing else about you; GitHub sees your IP address, as with any download from GitHub (see below). Nothing is downloaded or installed.

The programs you run inside nuntio, such as `ssh` or `curl`, do their own networking.

nuntio keeps these files on your computer, and they never leave it:

- your configuration file
- when the update check is used, the answer it got (`update.toml` in the same cache directory as the log file), so it doesn't ask again for a day
- a log file, only when nuntio is started without a terminal (for example `~/.cache/nuntio/nuntio.log` on Linux or `~/Library/Caches/nuntio/nuntio.log` on macOS)

nuntio uses the clipboard when you copy or paste, and when a program copies text with OSC 52 (the `clipboard_write` setting, which you can turn off). Programs running in nuntio can never read the clipboard.

## Website and downloads

The website at [cebor.github.io/nuntio](https://cebor.github.io/nuntio/) is a set of static pages. It sets no cookies, uses no analytics or tracking, and loads nothing from third parties.

The website and the downloads are hosted by GitHub (GitHub Pages and GitHub Releases). GitHub processes technical data such as your IP address to serve them; see the [GitHub General Privacy Statement](https://docs.github.com/en/site-policy/privacy-policies/github-general-privacy-statement).

## Contact

Felix Itzenplitz. Questions about this policy go to the [issue tracker](https://github.com/cebor/nuntio/issues) on GitHub.
