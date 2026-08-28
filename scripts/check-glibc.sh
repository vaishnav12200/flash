#!/bin/sh
set -eu

if [ "$#" -ne 2 ]; then
    echo "usage: $0 BINARY MAX_GLIBC_VERSION" >&2
    exit 2
fi

binary=$1
maximum=$2
if [ ! -f "$binary" ]; then
    echo "binary not found: $binary" >&2
    exit 1
fi

required=$(
    objdump -T "$binary" |
        sed -n 's/.*(GLIBC_\([0-9][0-9.]*\)).*/\1/p' |
        sort -Vu |
        tail -n 1
)
if [ -z "$required" ]; then
    echo "could not determine the required glibc version" >&2
    exit 1
fi

highest=$(printf '%s\n%s\n' "$required" "$maximum" | sort -V | tail -n 1)
if [ "$highest" != "$maximum" ]; then
    echo "binary requires glibc $required, newer than allowed $maximum" >&2
    exit 1
fi

echo "maximum required glibc version: $required (allowed: $maximum)"
