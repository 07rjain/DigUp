#!/bin/bash
# Builds DigUp's Windows installer: the Tauri shell (shell/) with DigUp's helper, llama.cpp, PDFium, the Swift and C++
# runtimes and ffmpeg in its helper folder, as an NSIS installer that installs for the current user (no admin prompt).
# Run it in Git for Windows' bash inside a Visual Studio developer environment, after scripts/fetch-vendor.sh and
# scripts/build-llama.sh. Needs Swift, Rust, Node (for Tauri's CLI), and an LGPL build of ffmpeg 8.1 or later with its
# DLLs (FFMPEG: its bin folder):
#   FFMPEG=D:/ffmpeg/bin scripts/package-windows.sh      → shell/src-tauri/target/release/bundle/nsis/
set -euo pipefail
cd "$(dirname "$0")/.."
FFMPEG=${FFMPEG:?set FFMPEG to the bin folder of an LGPL ffmpeg build (ffmpeg.exe, ffprobe.exe and their DLLs)}
TAURI_CLI=2.12.2

swift build -c release --product digup

# What the installer puts beside DigUp.exe, in helper\ ("DigUp.exe" and "digup.exe" can't share a folder): the helper
# finds the DLLs, llama.cpp's backends and ffmpeg in its own folder. The C++ runtime goes with them, so nothing needs
# installing first; the shell itself links its C runtime statically (.cargo/config.toml).
swift_root=$(cd "$(dirname "$(command -v swift)")/../../../.." && pwd)   # …/Swift from …/Swift/Toolchains/*/usr/bin
swift_runtime=$(ls -d "$swift_root"/Runtimes/*/usr/bin | tail -1)
crt=$(ls -d "$(cygpath "$VCToolsRedistDir")"x64/Microsoft.VC*.CRT | tail -1)   # set by the developer environment
helper=shell/src-tauri/helper
rm -rf "$helper"
mkdir -p "$helper"
cp .build/release/digup.exe Vendor/llama/lib/*.dll Vendor/pdfium/bin/pdfium.dll "$helper/"
cp "$swift_runtime"/*.dll "$helper/"
cp "$crt"/*.dll "$helper/"   # over the Swift runtime's copies of the same: the newer C++ runtime
cp "$FFMPEG"/ffmpeg.exe "$FFMPEG"/ffprobe.exe "$FFMPEG"/*.dll "$helper/"

# The helper folder goes in as the installer's resources here only: Tauri checks resources on every build.
(cd shell/src-tauri && npx --yes "@tauri-apps/cli@$TAURI_CLI" build --bundles nsis \
  --config '{"bundle":{"resources":{"helper/":"helper/"}}}')
ls -la shell/src-tauri/target/release/bundle/nsis/
