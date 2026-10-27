#!/bin/sh
# The Android build: target/android/openkumite.apk, for arm64 phones, tablets and TV boxes (and an arm64 emulator),
# signed with the release key when ANDROID_KEYSTORE names it (releases), else the SDK's debug key
# (~/.android/debug.keystore, made if missing). The game is libopenkumite.so in a
# NativeActivity (AndroidManifest.xml; OpenKumiteActivity.java, its one Java class, made into classes.dex with javac
# and d8: no Gradle). Needs the Android SDK (ANDROID_HOME: build-tools, a platform), its NDK (ANDROID_NDK_HOME),
# cargo-ndk (cargo install cargo-ndk) and a JDK (javac, keytool, apksigner).
# usage: sh packaging/android/build.sh
set -e
cd "$(dirname "$0")/../.."
: "${ANDROID_HOME:?the Android SDK}" "${ANDROID_NDK_HOME:?the Android NDK}"
tools=$(ls -d "$ANDROID_HOME"/build-tools/* | sort -V | tail -1)
platform=$(ls -d "$ANDROID_HOME"/platforms/android-[0-9]* | sort -V | tail -1)/android.jar
target=${CARGO_TARGET_DIR:-target}
rustup target add aarch64-linux-android >/dev/null 2>&1 || true
# (the library alone, as a shared one: the desktop build keeps it an rlib)
cargo ndk -t arm64-v8a -P 26 rustc --lib --release -p openkumite --crate-type cdylib
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
mkdir -p "$tmp/res/mipmap" "$tmp/apk/lib/arm64-v8a" "$target/android"
cp packaging/web/icon-192.png "$tmp/res/mipmap/icon.png"
cp "$target/aarch64-linux-android/release/libopenkumite.so" "$tmp/apk/lib/arm64-v8a/"
"$tools/aapt2" compile --dir "$tmp/res" -o "$tmp/res.zip"
"$tools/aapt2" link -o "$tmp/base.apk" --manifest packaging/android/AndroidManifest.xml -I "$platform" -R "$tmp/res.zip"
javac -nowarn --release 17 -classpath "$platform" -d "$tmp/classes" packaging/android/OpenKumiteActivity.java
"$tools/d8" --min-api 26 --lib "$platform" --output "$tmp/apk" $(find "$tmp/classes" -name '*.class')
(cd "$tmp/apk" && zip -q -r ../base.apk lib classes.dex)
"$tools/zipalign" -f -p 4 "$tmp/base.apk" "$tmp/aligned.apk"
# the release key (ANDROID_KEYSTORE, its ANDROID_KEYSTORE_PASSWORD and ANDROID_KEY_ALIAS: a release's APK updates an
# installed one only when signed by the same key), else the SDK's debug key (made if missing)
if [ -n "${ANDROID_KEYSTORE:-}" ]; then
    : "${ANDROID_KEYSTORE_PASSWORD:?the password of the release keystore}"
    "$tools/apksigner" sign --ks "$ANDROID_KEYSTORE" --ks-pass env:ANDROID_KEYSTORE_PASSWORD \
        --ks-key-alias "${ANDROID_KEY_ALIAS:-openkumite}" --out "$target/android/openkumite.apk" "$tmp/aligned.apk"
else
    key=$HOME/.android/debug.keystore
    if [ ! -f "$key" ]; then
        mkdir -p "$HOME/.android"
        keytool -genkeypair -keystore "$key" -storepass android -alias androiddebugkey -keypass android -keyalg RSA \
            -validity 10000 -dname "CN=Android Debug,O=Android,C=US"
    fi
    "$tools/apksigner" sign --ks "$key" --ks-pass pass:android --out "$target/android/openkumite.apk" "$tmp/aligned.apk"
fi
echo "built $target/android/openkumite.apk"
