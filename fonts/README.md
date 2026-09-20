# Fonts for the render test

This directory holds the font files that
[`widget/examples/render_test.rs`](../widget/examples/render_test.rs) cycles
through with its `font` button (or the `f` key). The example sends
`OSC 50 ; <path> ST` with a path below this directory, and Ratty loads that
file together with any bold/italic siblings next to it.

This branch exists only to keep the example, the OSC 50 font loading it
drives, and the fonts themselves somewhere. It is not meant to be merged: the
application does not need any of the three. It sits on top of #155, which
carries the renderer upgrade alone.

What this branch adds beyond #155:

- `unhandled_osc` in `src/runtime.rs`: `OSC 50 ; <name> ST` queues a font
  request; `?` queries and `#` font-menu indices are ignored.
- `LoadedFontFiles`, `load_font_file_faces`, and `sibling_font_faces` in
  `src/terminal.rs`: per-path font asset cache and bold/italic discovery.
- The switch in `pump_pty_output` and the revert-on-`FontFailed` half of
  `observe_renderer_status` in `src/systems.rs`.
- The `fonts` ignore entry, this directory, and the render test's font button.

Note that OSC 50 is writable by anything holding the PTY, so the file-loading
path lets terminal output pick which file gets read and parsed. That is why it
is not in the application.

## Expected layout

The paths are the second field of `FONTS` in the example and must match
exactly. A missing file is not an error the example can see: the path stops
looking like a file, Ratty treats it as a system family name, and the switch
either resolves to a fallback face or is reverted when the renderer reports
`FontFailed`.

| Label            | Path                                            | Source |
| ---------------- | ----------------------------------------------- | ------ |
| DejaVu Sans Mono | `dejavu/DejaVuSansMono.ttf`                     | <https://github.com/dejavu-fonts/dejavu-fonts/releases> |
| JetBrains Mono   | `jetbrains-mono/JetBrainsMono-Regular.ttf`      | <https://github.com/JetBrains/JetBrainsMono/releases> |
| Fira Code        | `fira-code/FiraCode-Regular.ttf`                | <https://github.com/tonsky/FiraCode/releases> |
| Iosevka          | `iosevka/Iosevka-Regular.ttf`                   | <https://github.com/be5invis/Iosevka/releases> |
| Menlo            | `menlo/Menlo.ttc`                               | macOS: `/System/Library/Fonts/Menlo.ttc` |
| SF Mono          | `sf-mono/SFNSMono.ttf`                          | macOS: `/System/Library/Fonts/SFNSMono.ttf` |
| Hack             | `hack/Hack-Regular.ttf`                         | <https://github.com/source-foundry/Hack/releases> |
| Source Code Pro  | `source-code-pro/SourceCodePro-Regular.ttf`     | <https://github.com/adobe-fonts/source-code-pro/releases> |
| Cascadia Code    | `cascadia-code/CascadiaCode-Regular.ttf`        | <https://github.com/microsoft/cascadia-code/releases> |

Sibling styles are optional. When `<base>-Bold`, `-Italic`/`-It`/`-Oblique`,
or `-BoldItalic`/`-BoldIt`/`-BoldOblique` sits next to the regular file with
the same extension, Ratty loads it instead of synthesizing that style.

Menlo and SF Mono ship with macOS and are not redistributable; copy them from
the system paths above on a macOS host. The remaining families are OFL or
Apache-2.0 and may be committed to this branch.
