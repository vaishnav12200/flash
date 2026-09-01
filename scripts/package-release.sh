#!/bin/sh
set -eu

if [ "$#" -lt 2 ] || [ "$#" -gt 3 ]; then
    echo "usage: $0 VERSION BINARY [DIST_DIR]" >&2
    exit 2
fi

version=$1
binary=$2
dist_dir=${3:-dist}
target=x86_64-unknown-linux-gnu
archive_base="flash-v${version}-${target}"
archive="$dist_dir/$archive_base.tar.gz"

case "$version" in
    *[!0-9.]* | .* | *.)
        echo "invalid release version: $version" >&2
        exit 2
        ;;
esac

if [ ! -x "$binary" ]; then
    echo "release binary is missing or not executable: $binary" >&2
    exit 1
fi

project_dir=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
dist_dir=$(mkdir -p -- "$dist_dir" && CDPATH= cd -- "$dist_dir" && pwd)
archive="$dist_dir/$archive_base.tar.gz"
stage=$(mktemp -d)
trap 'rm -rf -- "$stage"' EXIT HUP INT TERM
root="$stage/$archive_base"

install -Dm755 "$binary" "$root/bin/flash"
install -Dm644 "$project_dir/packaging/flash.desktop" \
    "$root/share/applications/flash.desktop"
install -Dm644 "$project_dir/README.md" "$root/README.md"
install -Dm644 "$project_dir/CHANGELOG.md" "$root/CHANGELOG.md"
install -Dm644 "$project_dir/LICENSE-MIT" "$root/LICENSE-MIT"
install -Dm644 "$project_dir/SECURITY.md" "$root/SECURITY.md"
release_notes="$project_dir/RELEASE_NOTES_v${version}.md"
if [ -f "$release_notes" ]; then
    install -Dm644 "$release_notes" "$root/RELEASE_NOTES.md"
fi
cp -R "$project_dir/contrib" "$root/contrib"

epoch=${SOURCE_DATE_EPOCH:-0}
find "$root" -exec touch -h -d "@$epoch" {} +
tar --sort=name --mtime="@$epoch" --owner=0 --group=0 --numeric-owner \
    -C "$stage" -czf "$archive" "$archive_base"
(
    cd "$dist_dir"
    sha256sum "$(basename -- "$archive")"
) > "$archive.sha256.tmp"
mv "$archive.sha256.tmp" "$archive.sha256"

echo "$archive"
echo "$archive.sha256"
