# OpenKumite

A 3D fighting game engine in Rust that plays converted game data 1:1, as the original does. It runs natively
(wgpu + winit) and in the browser (WebAssembly), with local two-player and online rollback netplay.

The repository contains **no game data**. Everything game-specific is produced once by
[kumite_converter](https://github.com/openkimute/kumite_converter) (a separate project) from your own copy of the game, as standard formats: glTF 2.0,
PNG, JSON, WAV/FLAC and AVI.

## Repository layout

| Path | What it is |
|---|---|
| `app/` | `openkumite`, the game: window, 60 Hz loop, input, menus and screens, HUD, modes, saves, audio, movies, online rooms, debug tools |
| `crates/kumite_sim/` | Deterministic 60 Hz fight simulation driven only by the converted JSON game data (no renderer) |
| `crates/kumite_game/` | Game layer tying the assets to the simulation (bouts, tag, Force mode, effects, sound); no GPU, so fights run headless |
| `crates/kumite_assets/` | Loads the converted assets: skinned glTF characters, stages, JSON game data, AVI movies |
| `crates/kumite_render/` | wgpu renderer: skinned characters, stage geometry and backdrop, debug lines |
| `crates/kumite_fs/` | File reads: the disk natively, the page's web server in a browser |
| `crates/kumite_net/` | Online wire formats: room messages, rollback packet framing, pad input, reliable UDP channel |
| `crates/serve/` | Small local web server for the browser build (Range requests, pre-compressed files) |
| `web/` | Browser build: `index.html` and the build / compress / serve / preload scripts |
| `fuzz/` | cargo-fuzz targets for everything read from files or the network |
| `vendor/matchbox_socket/` | Patched matchbox_socket (rustls instead of the default TLS backend) |
| `check.sh` | Formatting, clippy, tests, `cargo deny` and fuzzing, also run by the git hooks |

## Assets

Convert your own copy of the game with [kumite_converter](https://github.com/openkimute/kumite_converter), then put its output in `assets/` (git-ignored), or point
the game at it. The native game looks for the assets in this order:

1. `--assets DIR`
2. the `OPENKUMITE_ASSETS` environment variable
3. `assets/` next to the executable
4. per-OS places: on macOS the `.app`'s `Contents/Resources/assets` and `~/Library/Application Support/OpenKumite/assets`;
   on Windows `%APPDATA%\OpenKumite\assets`; on Linux `$XDG_DATA_HOME/openkumite/assets` (`~/.local/share`), then
   `$XDG_DATA_DIRS` (`/usr/local/share`, `/usr/share`)
5. the repository's `assets/`

A folder counts as a conversion when it holds `manifest.json`.

## Running natively

```sh
cargo run --release
cargo run --release -- --help     # the full command line
```

The game starts at the title screen. Modes include Arcade, VS, Team Battle, Time Attack, Survival, Force,
Practice and Watch CPU, plus OpenKumite's own Tag Brawl and Online.

### Controls

| | Move | Buttons 1 2 3 4 (LP RP LK RK) | Start |
|---|---|---|---|
| One player | arrows or WASD | U I J K | Enter |
| VS, player 1 | WASD | F G V B | |
| VS, player 2 | arrows | ; ' . / (or numpad 4 5 1 2) | |
| Gamepad | d-pad or stick | Square Triangle Cross Circle | Start (Select = back) |

Tap up/down to sidestep. Standing still or holding back guards highs and mids; crouching guards lows. Esc pauses.
Keys and pad buttons can be changed under OPTION (KEYBOARD, KEY CONFIGURATION).

### Debug and testing

F1 debug panel, F2 pause, F3 step a frame, F4 hit volumes, F5 reset round, F6/F7 recording and P2 behaviour,
F8 free camera, F9 infinite health, F10 video statistics, F12 screenshot, `-` / `=` speed, M sound.

Useful flags:

```sh
openkumite --fight --models A,B --stage N        # straight into a free fight
openkumite --shot out.png --frames 120           # headless screenshot
openkumite --screen select --shot select.png     # render a menu screen
openkumite --keys "30:start,60:b1,400:shot" --quit-after 600   # scripted run
```

Headless example tools live in `crates/kumite_game/examples/` (`cpu_match`, `trace`, `ko_trace`, ...).

## Browser build

```sh
sh web/build.sh          # wasm + JS glue into web/pkg (installs wasm-bindgen-cli on first use); --fast for a quicker build
sh web/serve.sh          # http://127.0.0.1:8770/ ; `sh web/serve.sh 8080 --bind 0.0.0.0` for a LAN
```

On Windows, `web\build.bat [--fast]` does the same (the compressed copies come from `web\compress.ps1`: gzip,
and brotli too under PowerShell 7); serve with `cargo run --release -p serve -- 8770 web`.

`web/serve.sh` links `web/assets` to `OPENKUMITE_ASSETS` (or `../assets`). `sh web/make_preload.sh` regenerates
`web/preload.json`, the files fetched before the first screens show.

The page takes the game data from the first of:

1. the copy the browser keeps: drop the folder kumite_converter wrote (the one with `manifest.json`) anywhere on the
   page, or pick it with **Choose folder…**. The page copies it into the browser's storage (OPFS, as `assets.pack` and
   `assets.idx`, the same pack kumite_converter's browser page writes), then starts. The **Game data** button over
   the game shows its size and whether the browser protects it from clean-ups, and replaces or removes it; dropping
   another folder on the game replaces it too.
2. a folder remembered in Chrome or Edge (kumite_converter's browser page writing to a folder on the disk)
3. the server's `assets/`, unless the page was built with `--no-server-assets`

`sh web/build.sh --no-server-assets` (`web\build.bat --no-server-assets`) builds a page that never reads the
server's `assets/`: every player brings their own copy. Every push to `main` deploys that build to GitHub Pages:
https://openkimute.github.io/openkimute/. kumite_converter's browser page at
https://openkimute.github.io/kumite_converter/ is on the same site, so a game it converts there is found by the
game's page directly.

The page is a PWA: the browser offers to install it (Chrome and Edge: the install icon in the address bar; Android:
**Add to Home screen**; iPhone and iPad: Share → **Add to Home Screen**), and the installed game opens full screen.
Its service worker (`web/sw.js`) keeps the page and the game itself, so after one visit online it also starts
offline. The game data is never in that cache: it stays the copy the browser keeps. The install icons are in
`packaging/web/`, made from the SVG by `sh packaging/icons.sh`.

## Online play

Players meet in a room on a [matchbox](https://github.com/johanhelsing/matchbox) signalling server and connect
over WebRTC; matches use rollback netcode (ggrs). The room's creator hosts it, with up to four players.

```sh
openkumite --online ROOM [--signal ws://HOST:PORT] [--name NICK] [--locals 1|2] [--host-room]
```

Without `--signal`, a local `matchbox_server` on port 3536 is used.

## Saves

Records, unlocks and settings are kept as JSON in the system's application data folder (macOS
`~/Library/Application Support/OpenKumite`, Windows `%APPDATA%\OpenKumite`, otherwise
`$XDG_DATA_HOME/openkumite`), or in local storage in a browser. `OPENKUMITE_SAVE_DIR` overrides the folder.

## Releases

A release's tag is `vX.Y.Z`, equal to the workspace version in `Cargo.toml` (`vX.Y.Z-suffix` for a pre-release).
Either push the tag (`git tag v1.2.3 && git push origin v1.2.3`: the release is made with GitHub's list of changes),
or publish a release on GitHub's Releases page (its title and text are kept). Either way
`.github/workflows/release.yml` builds every platform with the `dist` profile and, only when all of them built,
attaches the files below and adds a Downloads section to the release's text. Run by hand
(Actions → release → Run workflow) on any branch, it builds the same files without publishing them: they are the
run's artifacts, named `vX.Y.Z-dev.<commit>`.

| Download | Built as |
|---|---|
| `linux-amd64`, `linux-arm64` | glibc 2.28 baseline (any distribution since 2018); only libc, ALSA and libudev needed, the GPU and display libraries are loaded at start. `serve` is static (musl) |
| `.deb`, `.rpm`, `.pkg.tar.zst` (Arch) | that build as a package (`packaging/linux/nfpm.yaml`, made with nfpm): `/usr/bin/openkumite`, a desktop entry and icon, the docs and licenses |
| `.apk` (Alpine) | Alpine's own build, linked dynamically to musl (`packaging/linux/alpine.sh`), packaged the same way |
| `windows-amd64`, `windows-arm64` | C runtime linked in (`.cargo/config.toml`): no VC++ redistributable; the icon is in the `.exe` |
| `macos-universal` | `OpenKumite.app` (`packaging/macos/bundle.sh`): one arm64 + x86_64 binary with its icon, macOS 11+ |
| `web` | the browser build (`web/build.sh`) |

The game is not a static musl binary on Linux: a static program cannot load libraries, and the game needs the
system's Vulkan / OpenGL and X11 / Wayland libraries to draw.

The icon is `packaging/openkumite.svg` (Linux and the browser use it as it is); `sh packaging/icons.sh` makes the
Windows `.ico` and the macOS `.icns` from it again after a change.

## Development

```sh
sh check.sh commit   # rustfmt, clippy (native, wasm, fuzz) with -D warnings, tests
sh check.sh push     # the above and cargo deny (default)
sh check.sh all 60   # the above and every fuzz target for 60 s each
```

The git hooks (`.cargo-husky/hooks`) run `commit` and `push`. Tests read the converted assets from
`OPENKUMITE_ASSETS` (default `assets/`). The wasm lint needs `rustup target add wasm32-unknown-unknown`;
fuzzing needs nightly and `cargo-fuzz`.

Code rules (workspace lints): no `unsafe` (except the macOS haptics FFI), and no `unwrap` / `expect` / `panic!` on
anything that comes from outside: the game must never crash on bad files or network packets.

## License

OpenKumite's code is licensed under either of [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE), at your option.
See [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md) for the vendored code.
