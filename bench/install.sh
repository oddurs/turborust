#!/usr/bin/env sh
# Installs the tools bench/compare.py measures turborust against, from source,
# into ./bench/.tools — nothing is installed system-wide except through pip,
# gem and npm. Prints the PATH to use.
set -eu
here=$(cd "$(dirname "$0")" && pwd)
tools="$here/.tools"
mkdir -p "$tools"

GOPATH="$tools/go" go install github.com/f1bonacc1/process-compose@latest
GOPATH="$tools/go" go install github.com/DarthSim/hivemind@latest
GOPATH="$tools/go" go install github.com/DarthSim/overmind/v2@latest   # needs tmux
cargo install --root "$tools/cargo" watchexec-cli
pip3 install honcho
gem install --no-document foreman
npm install -g nodemon

echo "export PATH=\"$tools/go/bin:$tools/cargo/bin:\$PATH:$(ruby -e 'puts Gem.bindir')\""
