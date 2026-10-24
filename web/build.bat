@echo off
rem Windows' web/build.sh: build the browser version: web\pkg\ (wasm + JS glue), then the compressed copies the server
rem sends (web\compress.ps1). Needs the wasm32 target and wasm-bindgen-cli 0.2.129 (installed into target\tools on first
rem use). The `web` profile (whole-program optimisation, ~2 min) is the smaller download; --fast builds the release
rem profile instead (for trying changes). --no-server-assets: the page never reads the server's assets\, only the
rem player's own copy (dropped on the page, kept in the browser): for a public site such as GitHub Pages.
setlocal
cd /d "%~dp0.."
set profile=web
set server_assets=true
:args
if "%~1"=="" goto built_args
if "%~1"=="--fast" (set profile=release) else if "%~1"=="--no-server-assets" (set server_assets=false) else (
  echo usage: web\build.bat [--fast] [--no-server-assets] & exit /b 1
)
shift
goto args
:built_args
rustup target add wasm32-unknown-unknown >nul 2>&1
if not exist target\tools\bin\wasm-bindgen.exe (
  cargo install wasm-bindgen-cli --version 0.2.129 --root target\tools || exit /b 1
)
cargo build --profile %profile% --target wasm32-unknown-unknown -p openkumite || exit /b 1
target\tools\bin\wasm-bindgen.exe --target web --no-typescript --out-dir web\pkg "target\wasm32-unknown-unknown\%profile%\openkumite.wasm" || exit /b 1
rem this build's name (index.html fetches the game by it: no browser keeps an old copy)
set version=
for /f %%h in ('powershell -NoProfile -Command "(Get-FileHash -Algorithm SHA256 web\pkg\openkumite_bg.wasm).Hash.ToLower().Substring(0, 16)"') do set version=%%h
if not defined version (echo no version for the build ^(Get-FileHash^) & exit /b 1)
powershell -NoProfile -Command "[IO.File]::WriteAllText('web\pkg\version.txt', '%version%' + [char]10)" || exit /b 1
rem the page's settings (index.html)
>web\pkg\config.json echo {"serverAssets": %server_assets%}
rem the page's icon
copy /y packaging\openkumite.svg web\pkg\ >nul || exit /b 1
rem (PowerShell 7 when there is one: it also makes the brotli copies)
set ps=powershell
where pwsh >nul 2>&1 && set ps=pwsh
%ps% -NoProfile -ExecutionPolicy Bypass -File web\compress.ps1 || exit /b 1
echo built web\pkg (%profile%); serve with: cargo run --release -p serve -- 8770 web
