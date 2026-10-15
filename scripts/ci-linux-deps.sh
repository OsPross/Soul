#!/usr/bin/env bash
# System packages needed to build (and bundle) Soul on Ubuntu 22.04 — used by CI and handy locally.
set -euo pipefail
sudo apt-get update
sudo apt-get install -y --no-install-recommends \
  build-essential pkg-config curl file patchelf \
  libwebkit2gtk-4.1-dev libgtk-3-dev libsoup-3.0-dev libjavascriptcoregtk-4.1-dev \
  libayatana-appindicator3-dev librsvg2-dev \
  libasound2-dev libxdo-dev libssl-dev \
  libx11-dev libxext-dev libxtst-dev libxrandr-dev libxi-dev libxfixes-dev \
  libxcb1-dev libxcb-randr0-dev libxcb-shm0-dev libxcb-xfixes0-dev libxcb-record0-dev libxkbcommon-dev \
  libdbus-1-dev \
  rpm
