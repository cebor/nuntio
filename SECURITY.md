# Security policy

## Supported versions

Only the [latest release](https://github.com/cebor/nuntio/releases/latest) gets security fixes. Please check that a problem still occurs there before reporting it.

## Reporting a vulnerability

Please don't open a public issue for security problems. Instead:

- use [private vulnerability reporting](https://github.com/cebor/nuntio/security/advisories/new) on GitHub ("Report a vulnerability" in the Security tab), or
- email felix+nuntio@stkn.org.

Include the nuntio version, your OS, and the steps or a file that reproduces the problem (for example the bytes a program prints to trigger it).

You'll get an answer within a few days. Once the problem is confirmed, the fix ships in a patch release, and the release notes credit you unless you'd rather not be named.

## Scope

nuntio shows whatever programs print, including output from remote hosts and untrusted files, so the terminal itself is the main attack surface. Examples of what counts:

- escape sequences that crash or hang nuntio, run commands, or read or write files
- abuse of the clipboard, e.g. through OSC 52
- links or paths that open something other than what the user clicked
- problems in the installers and packages

Bugs in the programs you run inside nuntio, such as your shell, belong to those projects.
