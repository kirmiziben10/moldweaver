#!/usr/bin/env bash
# Build (and optionally install) the MoldWeaver Flatpak.
#
#   flatpak/build.sh            build into flatpak/repo and make a bundle
#   flatpak/build.sh --install  also install it for the current user
#
# Needs: flatpak, flatpak-builder, cargo (on the host, to vendor crates).
set -euo pipefail
cd "$(dirname "$0")/.."
ROOT=$(pwd)
APP_ID=io.github.moldweaver.MoldWeaver

echo ">> Installing the runtime, SDK and Rust extension (if missing)"
flatpak remote-add --user --if-not-exists flathub https://dl.flathub.org/repo/flathub.flatpakrepo
flatpak install --user --noninteractive flathub \
    org.freedesktop.Platform//25.08 \
    org.freedesktop.Sdk//25.08 \
    org.freedesktop.Sdk.Extension.rust-stable//25.08

echo ">> Vendoring Rust crates for an offline build"
rm -rf flatpak/vendor
cargo vendor --locked --versioned-dirs flatpak/vendor > flatpak/cargo-vendor-config.toml.tmp
# Point the vendored source at the path inside the Flatpak build directory.
sed 's#directory = ".*"#directory = "flatpak/vendor"#' flatpak/cargo-vendor-config.toml.tmp > flatpak/cargo-vendor-config.toml
rm flatpak/cargo-vendor-config.toml.tmp

echo ">> Building the Flatpak"
INSTALL=()
if [[ "${1:-}" == "--install" ]]; then
    INSTALL=(--user --install)
fi
flatpak-builder --force-clean --repo=flatpak/repo "${INSTALL[@]}" \
    flatpak/build-dir flatpak/$APP_ID.yml

echo ">> Creating a single-file bundle"
flatpak build-bundle flatpak/repo flatpak/moldweaver.flatpak $APP_ID
echo "Done: $ROOT/flatpak/moldweaver.flatpak"
echo "Run with: flatpak run $APP_ID"
