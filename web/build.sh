#!/bin/sh
# Build the browser version: web/pkg/ (wasm + JS glue), then the compressed copies the server sends
# (web/compress.sh). Needs the wasm32 target and wasm-bindgen-cli 0.2.129 (installed into target/tools on first
# use). The `web` profile (whole-program optimisation, ~2 min) is the smaller download; --fast builds the release
# profile instead (for trying changes). --no-server-assets: the page never reads the server's assets/, only the
# player's own copy (dropped on the page, kept in the browser): for a public site such as GitHub Pages.
set -e
cd "$(dirname "$0")/.."
profile=web
server_assets=true
for a in "$@"; do
  case "$a" in
    --fast) profile=release ;;
    --no-server-assets) server_assets=false ;;
    *) echo "usage: sh web/build.sh [--fast] [--no-server-assets]" >&2; exit 1 ;;
  esac
done
rustup target add wasm32-unknown-unknown >/dev/null 2>&1 || true
[ -x target/tools/bin/wasm-bindgen ] || cargo install wasm-bindgen-cli --version 0.2.129 --root target/tools
cargo build --profile "$profile" --target wasm32-unknown-unknown -p openkumite
target/tools/bin/wasm-bindgen --target web --no-typescript --out-dir web/pkg "target/wasm32-unknown-unknown/$profile/openkumite.wasm"
# this build's name (index.html fetches the game by it: no browser keeps an old copy)
version=$(shasum -a 256 web/pkg/openkumite_bg.wasm)
version=${version%% *}
[ ${#version} -eq 64 ] || { echo "no version for the build (shasum)"; exit 1; }
printf '%s\n' "$version" | cut -c1-16 > web/pkg/version.txt
# the page's settings (index.html)
printf '{"serverAssets": %s}\n' "$server_assets" > web/pkg/config.json
# the page's icon
cp packaging/openkumite.svg web/pkg/
sh web/compress.sh
echo "built web/pkg ($profile); serve with web/serve.sh"
