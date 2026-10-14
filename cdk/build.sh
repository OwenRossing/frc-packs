#!/usr/bin/env bash
set -euo pipefail

repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cdk_dir="$repo/cdk"
build_dir="$cdk_dir/.build"
lambda_binary="$repo/server/target/lambda/frc-packs-server/bootstrap"

if ! command -v cargo-lambda >/dev/null 2>&1; then
  echo "Install cargo-lambda first: cargo install cargo-lambda" >&2
  exit 1
fi

(cd "$repo/web" && npm run build)
(cd "$repo/server" && cargo lambda build --release --features lambda --arm64)

test -x "$lambda_binary" || {
  echo "cargo-lambda did not produce the expected executable: $lambda_binary" >&2
  exit 1
}

rm -rf "$build_dir"
mkdir -p "$build_dir/site" "$build_dir/lambda/data"
cp -a "$repo/web/dist/." "$build_dir/site/"
cp -a "$repo/data/photos" "$build_dir/site/photos"
cp -a "$repo/data/packs" "$build_dir/site/packs"
cp "$lambda_binary" "$build_dir/lambda/bootstrap"
cp -a "$repo/data/packs" "$build_dir/lambda/data/packs"
chmod 755 "$build_dir/lambda/bootstrap"

echo "Built site and Lambda assets in $build_dir"
