#!/usr/bin/env bash

set -euo pipefail

export CARGO_PROFILE_RELEASE_STRIP=symbols

cargo-bundle-licenses --format yaml --output THIRDPARTY.yml

cargo install --verbose --locked --no-track --root "$PREFIX" --path .
