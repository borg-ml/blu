#!/usr/bin/env bash
set -euo pipefail

repository_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
download_dir="$repository_root/.upstream/downloads"
source_dir="$repository_root/.upstream/lua"
checksum_file="$repository_root/upstream/lua.sha256"
versions=(5.1.5 5.2.4 5.3.6 5.4.8 5.5.0)

mkdir -p "$download_dir" "$source_dir"

for version in "${versions[@]}"; do
    archive="lua-$version.tar.gz"
    archive_path="$download_dir/$archive"
    if [[ ! -f "$archive_path" ]]; then
        curl --fail --location --silent --show-error \
            "https://www.lua.org/ftp/$archive" \
            --output "$archive_path"
    fi
done

(
    cd "$download_dir"
    sha256sum --check "$checksum_file"
)

for version in "${versions[@]}"; do
    archive="lua-$version.tar.gz"
    checkout="$source_dir/lua-$version"
    if [[ ! -d "$checkout" ]]; then
        tar --extract --gzip --file "$download_dir/$archive" --directory "$source_dir"
    fi
    make --directory "$checkout" linux
done
