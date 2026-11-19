# Terminal — an Ardoise Editor plugin

Shells in tabs under the editor of [Ardoise Editor](https://github.com/ardoise-editor/ardoise): your `$SHELL` in a pseudo-terminal (xterm-256color, true color, the mouse for vim or htop), its history with search, and `file:line` opened with Cmd/Ctrl+click. Copy and paste with Ctrl+Shift+C / V (Cmd+C / V on macOS). **Open in Terminal** in the menu of a folder starts one there.

## Build

```sh
cargo test
cargo build --release && mkdir -p bin && cp target/release/terminal bin/
```

## License

MIT
