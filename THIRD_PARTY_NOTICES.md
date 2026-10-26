# Third-party notices

OpenKumite's own code is under the [MIT licence](LICENSE-MIT). It contains, or is derived from, the following third-party
work. The Rust crates it depends on are under their own licences
(MIT, Apache-2.0, BSD, Zlib, MPL-2.0, ...), checked by `cargo deny` against `deny.toml`.

## matchbox_socket 0.14 (`vendor/matchbox_socket/`)

A patched copy of [matchbox_socket](https://github.com/johanhelsing/matchbox) by Johan Helsing and the matchbox
contributors, licensed MIT OR Apache-2.0; used here under the MIT licence ([its text](vendor/matchbox_socket/LICENSE-MIT)).
The patch replaces its TLS backend with rustls.

## winit 0.30 (`vendor/winit/`)

A patched copy of [winit](https://github.com/rust-windowing/winit) by the winit contributors, licensed Apache-2.0
([its text](vendor/winit/LICENSE)). The patch leaves a gamepad's stick and hat motion on Android to the system.

## Big Shoulders Display (`web/fonts/`)

The browser version's display typeface, [Big Shoulders Display](https://github.com/xotypeco/big_shoulders) by
Patric King (XO Type Co), licensed under the SIL Open Font License 1.1 ([its text](web/fonts/OFL.txt)). The file is
the variable font's Latin subset, as packaged by [Fontsource](https://fontsource.org).

## No game data

The repository contains no game data. Everything game-specific is
read by the converter (kumite_converter, a separate project) from the user's own copies, and by OpenKumite from the
converter's output.
