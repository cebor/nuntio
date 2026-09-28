# Code signing policy

Free code signing provided by [SignPath.io](https://about.signpath.io/), certificate by [SignPath Foundation](https://signpath.org/).

## What is signed

On Windows, these files from each [release](https://github.com/cebor/nuntio/releases) are signed:

- `nuntio.exe` and `nuntio-config.exe`, in the `.zip` and in the installer
- the installer, `nuntio-<version>-x86_64-windows-setup.exe`

They are built from the source code in [cebor/nuntio](https://github.com/cebor/nuntio) by the [release workflow](https://github.com/cebor/nuntio/blob/main/.github/workflows/release.yml) on GitHub Actions, which runs when a version tag is pushed. Nothing built elsewhere, such as on a developer's machine, is signed. Releases up to 0.1.5 are unsigned.

The packages for macOS and Linux are not signed. Every release lists the SHA-256 checksums of all files in `SHA256SUMS`.

## Team roles

- Committers and reviewers: [Felix Itzenplitz](https://github.com/cebor)
- Approvers: [Felix Itzenplitz](https://github.com/cebor)

Changes from other contributors are reviewed by a committer before they are merged. Every signing request is approved by hand.

## Privacy policy

This program will not transfer any information to other networked systems unless specifically requested by the user or the person installing or operating it. See the [privacy policy](https://cebor.github.io/nuntio/privacy/) for details.
