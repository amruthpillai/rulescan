#!/bin/sh
# Install a verified RuleScan release for the current user; no service is installed.
set -eu

fail() { printf 'RuleScan installer: %s\n' "$*" >&2; exit 1; }
for tool in curl tar awk mktemp; do
    command -v "$tool" >/dev/null 2>&1 || fail "Required command is missing: $tool"
done

os=$(uname -s)
arch=$(uname -m)
case "$os:$arch" in
    Darwin:x86_64)
        # Prefer the native build when this shell itself runs under Rosetta.
        if [ "$(sysctl -in sysctl.proc_translated 2>/dev/null || true)" = 1 ]; then arch=arm64; fi
        ;;
esac
case "$os:$arch" in
    Darwin:arm64|Darwin:aarch64) target=aarch64-apple-darwin ;;
    Darwin:x86_64) target=x86_64-apple-darwin ;;
    Linux:x86_64|Linux:amd64) target=x86_64-unknown-linux-musl ;;
    Linux:aarch64|Linux:arm64) target=aarch64-unknown-linux-musl ;;
    *) fail "Unsupported platform $os/$arch. Windows users should use install.ps1." ;;
esac

repo=https://github.com/amruthpillai/rulescan
version=${RULESCAN_VERSION:-latest}
if [ "$version" = latest ]; then
    resolved=$(curl --proto '=https' --tlsv1.2 --fail --silent --show-error --location \
        --output /dev/null --write-out '%{url_effective}' "$repo/releases/latest")
    case "$resolved" in "$repo/releases/tag/"*) version=${resolved##*/} ;; *) fail "Cannot resolve the latest release." ;; esac
fi
case "$version" in v[0-9]*) ;; *) fail 'Version must be a release tag such as v0.1.0.' ;; esac
case "$version" in *[!A-Za-z0-9.-]*) fail 'Version contains unsupported characters.' ;; esac

if [ -n "${RULESCAN_INSTALL_DIR:-}" ]; then
    install_dir=$RULESCAN_INSTALL_DIR
else
    [ -n "${HOME:-}" ] || fail 'Set RULESCAN_INSTALL_DIR to an absolute destination.'
    install_dir=$HOME/.local/bin
fi
case "$install_dir" in /*) ;; *) fail 'RULESCAN_INSTALL_DIR must be absolute.' ;; esac
binary=$install_dir/rulescan
notices=$install_dir/rulescan-notices.txt
for destination in "$binary" "$notices"; do
    [ ! -L "$destination" ] && [ ! -d "$destination" ] || fail "Refusing to replace a link or directory: $destination"
done

umask 077
temporary=$(mktemp -d "${TMPDIR:-/tmp}/rulescan-install.XXXXXXXX")
staged_binary=
staged_notices=
cleanup() {
    [ -z "$staged_binary" ] || rm -f "$staged_binary"
    [ -z "$staged_notices" ] || rm -f "$staged_notices"
    rm -rf "$temporary"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
asset=rulescan-$version-$target.tar.gz
release=$repo/releases/download/$version
download() {
    curl --proto '=https' --tlsv1.2 --fail --silent --show-error --location \
        --max-filesize 134217728 --output "$2" "$1"
}
printf 'Downloading RuleScan %s (%s)…\n' "$version" "$target"
download "$release/$asset" "$temporary/archive.tar.gz"
download "$release/SHA256SUMS" "$temporary/SHA256SUMS"
expected=$(awk -v name="$asset" '$2 == name { count++; value=$1 } END { if (count == 1 && length(value) == 64 && value !~ /[^0-9a-f]/) print value; else exit 1 }' "$temporary/SHA256SUMS") \
    || fail 'Release checksum is missing, duplicated, or invalid.'
if command -v sha256sum >/dev/null 2>&1; then
    actual=$(sha256sum "$temporary/archive.tar.gz" | awk '{print $1}')
elif command -v shasum >/dev/null 2>&1; then
    actual=$(shasum -a 256 "$temporary/archive.tar.gz" | awk '{print $1}')
else
    fail 'Required command is missing: sha256sum or shasum.'
fi
[ "$actual" = "$expected" ] || fail 'SHA-256 mismatch; nothing was installed.'

# Even a checksummed package must have exactly the documented flat regular files.
tar -tzf "$temporary/archive.tar.gz" > "$temporary/members"
awk '
    $0 != "rulescan" && $0 != "README.md" && $0 != "THIRD_PARTY_NOTICES.txt" && $0 != "manifest.json" { exit 1 }
    ++seen[$0] != 1 { exit 1 }
    END { if (NR != 4) exit 1 }
' "$temporary/members" || fail 'Archive has unexpected, duplicate, or missing entries.'
tar -tvzf "$temporary/archive.tar.gz" > "$temporary/types"
awk 'substr($0,1,1) != "-" { exit 1 }' "$temporary/types" || fail 'Archive contains a link or other non-regular entry.'
mkdir "$temporary/extracted"
tar -xzf "$temporary/archive.tar.gz" --no-same-owner --no-same-permissions -C "$temporary/extracted"
chmod 755 "$temporary/extracted/rulescan"
[ "$("$temporary/extracted/rulescan" --version)" = "rulescan ${version#v}" ] \
    || fail 'The downloaded executable does not report the requested version.'

mkdir -p "$install_dir"
staged_binary=$(mktemp "$install_dir/.rulescan.XXXXXXXX")
staged_notices=$(mktemp "$install_dir/.rulescan-notices.XXXXXXXX")
cp "$temporary/extracted/rulescan" "$staged_binary"
cp "$temporary/extracted/THIRD_PARTY_NOTICES.txt" "$staged_notices"
chmod 755 "$staged_binary"
chmod 644 "$staged_notices"
for destination in "$binary" "$notices"; do
    [ ! -L "$destination" ] && [ ! -d "$destination" ] || fail "Destination became a link or directory: $destination"
done
if [ "$os" = Linux ]; then
    mv -fT "$staged_notices" "$notices"
    staged_notices=
    mv -fT "$staged_binary" "$binary"
else
    mv -fh "$staged_notices" "$notices"
    staged_notices=
    mv -fh "$staged_binary" "$binary"
fi
staged_binary=
printf 'Installed RuleScan %s to %s\n' "$version" "$binary"
case ":${PATH:-}:" in
    *":$install_dir:"*) printf 'Run: rulescan\n' ;;
    *) printf 'Run: %s\nAdd %s to your PATH to use the rulescan command.\n' "$binary" "$install_dir" ;;
esac
