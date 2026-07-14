# Bundled fonts

The Frost theme (`crates/ymir-gui/src/theme.rs`) uses **IBM Plex**, embedded into the binary via
`include_bytes!` in `crates/ymir-gui/src/main.rs`:

- **IBM Plex Sans** — Regular / Medium / SemiBold — the UI typeface.
- **IBM Plex Mono** — Regular / Medium — values, port labels, search fields, status.

## License

IBM Plex is licensed under the **SIL Open Font License, Version 1.1** (OFL-1.1), which permits
bundling and redistribution with the application. Source and full license text:
<https://github.com/IBM/plex>.

The OFL requires the license to accompany the fonts. The full license text (with IBM's copyright and
reserved font name) is in [`OFL.txt`](OFL.txt) alongside the fonts, so the bundle is self-describing.
