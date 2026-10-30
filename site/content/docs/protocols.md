+++
title = "Protocols"
description = "Which escape sequences and terminal protocols nuntio supports, and which it doesn't."
weight = 3
+++

nuntio parses the usual xterm sequences through [alacritty_terminal](https://crates.io/crates/alacritty_terminal) and adds its own handling for images, the Kitty keyboard protocol, paste events, working directories and links. This page lists what is supported, what is partly supported and what is not. `●` means supported, `◐` partly, `○` not at all.

## Graphics

Images become ordinary cells in the grid, so they scroll, clear and get overwritten like text, in splits and also in [WSL](@/docs/wsl.md). Images replace the text in their cells: z-order against text is not drawn, and the `z` key only selects placements for deletion.

| Protocol | Support | Details |
|---|---|---|
| iTerm2 inline images (OSC 1337 `File=`) | ● Yes | Needs `inline=1`; without it the sequence is ignored. PNG, JPEG, GIF (first frame), WebP and BMP, upright as the EXIF orientation says. `width` and `height` in cells, pixels (`px`), percent (`%`) or `auto`, and `preserveAspectRatio`. `name` and `size` are ignored. A payload over 64 MiB is dropped; bitmaps over 4095 pixels on a side are scaled down. |
| Kitty graphics: transmit and place | ● Yes | `a=t`, `a=T` and `a=p`. Pixel formats `f=24`, `f=32` and PNG (`f=100`), zlib compression (`o=z`), chunked transfers (`m=1`). Cropping with `x`, `y`, `w`, `h`, a pixel offset with `X`, `Y`, scaling over `c` columns and `r` rows, `C=1` to keep the cursor. Images are named with `i` or `I`, placements with `p`. Raw pixel data may be up to 16384 pixels on a side. |
| Kitty graphics: delete and query | ● Yes | `a=d` with the selectors `a`, `i`, `n`, `c`, `p`, `q`, `x`, `y`, `z` and `r`; an uppercase selector also frees the image data. `a=q` checks an image without placing it. Replies (`OK` or an error such as `EINVAL`) are sent for commands with an `i` or `I` key; `q=1` and `q=2` silence them. |
| Kitty graphics: files and shared memory (`t=f`, `t=t`, `t=s`) | ○ No | Only direct data (`t=d`). Other media are answered with `EINVAL`. |
| Kitty graphics: Unicode placeholders (`U=1`) | ○ No | Answered with `EINVAL`. |
| Kitty graphics: relative placements (`P`, `Q`, `H`, `V`) | ○ No | Answered with `EINVAL`. |
| Kitty graphics: animation (`a=f`, `a=a`, `a=c`) | ○ No | Answered with `EINVAL`. |
| Kitty graphics: indexed colour (`f=8`, `f=16`) | ○ No | Images whose pixels are indices into the terminal's palette. Answered with `EINVAL`. |
| Kitty graphics: feature queries (`a=q` with `e`) | ○ No | The `e` key is ignored: the query is answered like a plain `a=q`, without `e` in the reply, so programs fall back to other ways of detecting features. |
| Sixel | ○ No | |
| Cell size in pixels (`CSI 16 t`) | ● Yes | Answered with `CSI 6 ; height ; width t`, so programs can scale images to the grid. |

omp picks an image protocol on its own: nuntio sets `PI_FORCE_IMAGE_PROTOCOL=kitty` (see [Environment](#environment)).

## Keyboard and mouse

| Protocol | Support | Details |
|---|---|---|
| Kitty keyboard protocol | ● Yes | All five flags: disambiguate escape codes, report event types (press, repeat, release), report alternate keys, report all keys as escape codes, report associated text. A program turns them on and off itself; the `kitty_keyboard` setting (default on) can disable the protocol. |
| Bracketed paste (DEC 2004) | ● Yes | Pasted text is wrapped in `ESC [ 200 ~` and `ESC [ 201 ~`, with escape characters removed. Without it, line breaks become carriage returns and nuntio can ask before pasting several lines (`confirm_paste`). |
| Focus events (DEC 1004) | ● Yes | `CSI I` and `CSI O` when the window gains or loses focus. |
| Mouse reporting | ● Yes | Modes 1000 (clicks), 1002 (drag) and 1003 (all motion), in the default X10 encoding, UTF-8 (1005) and SGR (1006). |
| Input method (IME) | ● Yes | The uncommitted text is drawn underlined at the cursor; committed text is sent as typed. |

## Clipboard, links and working directory

| Protocol | Support | Details |
|---|---|---|
| Clipboard write (OSC 52) | ◐ Partly | Write only, controlled by `clipboard_write` (default on). A program that asks for the clipboard contents gets no answer, so it can't read what you copied. |
| Paste events (mode 5522, OSC 5522) | ◐ Partly | A program that turns on mode 5522 (`CSI ? 5522 h`), like omp, gets a paste as an OSC 5522 announcement with a one-time password instead of the text, and reads the data with it within 30 seconds: text as `text/plain`, a clipboard image as `image/png`. Pasted images reach it without a temporary file, so they also work over SSH and in [WSL](@/docs/wsl.md). Only the process group that turned the mode on gets paste events. Other programs keep bracketed paste, and an image is pasted as the path of a PNG file. The rest of the kitty clipboard protocol is refused: other reads and all writes are answered with `EPERM`. `CSI ? 5522 $ p` reports the mode; in cmd and PowerShell panes on Windows, and in WSL panes that fell back to ConPTY, it reports the mode as not supported. |
| Hyperlinks (OSC 8) | ● Yes | Ctrl-click (⌘-click on macOS) opens the link; while Ctrl (⌘) is held, hovering shows where the link leads. Only the schemes `https`, `http`, `file`, `ftp`, `ssh`, `git` and `mailto` are links. Links to programs are not opened. |
| Plain URLs | ● Yes | Found in the text without OSC 8, with the same schemes and the same click. |
| Plain paths | ● Yes | Absolute paths in the text (`/x`, `~/x`, `C:\x`, `C:/x`) open with the system's opener on the same click; a `:line` suffix is not part of the link. In [WSL](@/docs/wsl.md) panes on Windows, Linux paths open through the distribution's share and `/mnt/c/…` as `C:\…`. Relative paths (a word, or the content of quotes as `ls` writes names with spaces) are links only if the file exists in the pane's current working directory; in a pane running herdr or tmux (with `herdr.enabled` or `tmux.enabled`), in the directory of its focused pane, which nuntio asks for once Ctrl (⌘) is held for a moment without another key, or the pointer moves while it is held. In cmd and PowerShell panes on Windows, only if the shell reports its directory (OSC 7 or OSC 9;9, see below). Missing files and programs are not opened. |
| Working directory (OSC 7, OSC 9;9) | ● Yes | `file://host/path` (OSC 7) and the ConEmu and Windows Terminal form (OSC 9;9). On Linux and macOS nuntio reads the directory of the foreground process and uses the reported one as a fallback; on Windows and in [WSL](@/docs/wsl.md) it is the only source. Splits, and new tabs with `tabs.inherit_directory`, start there. |

## Screen and text

| Protocol | Support | Details |
|---|---|---|
| Synchronized output (DEC 2026) | ● Yes | Output between `CSI ? 2026 h` and `CSI ? 2026 l` is drawn at once. An update that is never closed is applied after 150 ms. |
| True colour | ● Yes | SGR 38 and 48 with RGB values. `COLORTERM=truecolor` is set. |
| Underline styles | ● Yes | Single, double, curly, dotted and dashed (SGR 4 with a sub-parameter), in the colour set with SGR 58. |
| Colour queries (OSC 4, 10, 11, 12) | ● Yes | Answered from the current theme colours. |
| Window and tab titles (OSC 0, OSC 2) | ● Yes | Shown in the tab, depending on `tabs.title`. |
| Bell | ◐ Partly | A bell in a background tab puts a dot in its label. While the tab or the window is out of view, nuntio also asks the system to draw attention to the window. nuntio plays no sound. |
| Desktop notifications (OSC 9, OSC 777, OSC 99) | ○ No | These sequences are ignored. OSC 9;9 is not a notification but a working directory report (see above). |

## Environment

nuntio sets these variables in every pane:

| Variable | Support | Details |
|---|---|---|
| `TERM` | ● Yes | `xterm-256color`. nuntio ships no terminfo entry of its own. |
| `COLORTERM` | ● Yes | `truecolor`. |
| `TERM_PROGRAM` | ● Yes | `nuntio`. |
| `TERM_PROGRAM_VERSION` | ● Yes | The version of nuntio. |
| `PI_FORCE_IMAGE_PROTOCOL` | ◐ Partly | `kitty`, so omp shows images inline. Empty in native Windows panes (cmd, PowerShell) and in WSL panes that fall back to ConPTY, where inline images don't work. |

On Windows, nuntio adds these variables to `WSLENV` so they reach programs inside WSL. See the [configuration reference](@/docs/config.md) for the settings named here.
