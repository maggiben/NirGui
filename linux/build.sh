#!/bin/sh
# Build Linux amd64 bundles in Docker; outputs land in linux/out/
set -e
cd "$(dirname "$0")/.."
docker build --platform linux/amd64 -f linux/Dockerfile -t nirscan-nano-linux .
rm -rf linux/out && mkdir -p linux/out
id=$(docker create --platform linux/amd64 nirscan-nano-linux)
docker cp "$id:/src/src-tauri/target/release/bundle/deb" linux/out/
docker cp "$id:/src/src-tauri/target/release/bundle/appimage" linux/out/ || true
docker cp "$id:/src/src-tauri/target/release/nano-cli" linux/out/ || true
docker rm "$id" >/dev/null
ls -lR linux/out
