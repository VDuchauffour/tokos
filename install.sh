#!/usr/bin/env bash
# Install the prebuilt tokos binary from GitHub Releases.
#
# Downloads the correct prebuilt binary for the user's OS/arch from
# https://github.com/VDuchauffour/tokos/releases, extracts it, and installs it.
# No cargo, no clone, no Rust toolchain required.
#
# Usage: install.sh [options]
#
# Options:
#   --prefix <dir>     Install under <dir>/bin (default: /usr/local).
#   --to <dir>         Install the binary directly into <dir> (overrides --prefix).
#   --version <tag>    Install a specific release tag (default: latest).
#   -h, --help         Print this help and exit.
#
# Examples:
#   ./install.sh
#   ./install.sh --prefix "$HOME/.local"
#   ./install.sh --to /opt/tokos/bin
#   ./install.sh --version v0.1.0
set -euo pipefail

REPO="VDuchauffour/tokos"

usage() {
	cat <<'EOF'
Usage: install.sh [options]

Options:
  --prefix <dir>     Install under <dir>/bin (default: /usr/local).
  --to <dir>         Install the binary directly into <dir> (overrides --prefix).
  --version <tag>    Install a specific release tag (default: latest).
  -h, --help         Print this help and exit.

Examples:
  ./install.sh
  ./install.sh --prefix "$HOME/.local"
  ./install.sh --to /opt/tokos/bin
  ./install.sh --version v0.1.0
EOF
}

PREFIX="/usr/local"
TO_DIR=""
VERSION="latest"

while [[ $# -gt 0 ]]; do
	case "$1" in
		--prefix)
			if [[ $# -lt 2 ]]; then
				echo "error: --prefix requires a value" >&2
				usage >&2
				exit 1
			fi
			PREFIX="$2"
			shift 2
			;;
		--to)
			if [[ $# -lt 2 ]]; then
				echo "error: --to requires a value" >&2
				usage >&2
				exit 1
			fi
			TO_DIR="$2"
			shift 2
			;;
		--version)
			if [[ $# -lt 2 ]]; then
				echo "error: --version requires a value" >&2
				usage >&2
				exit 1
			fi
			VERSION="$2"
			shift 2
			;;
		-h|--help)
			usage
			exit 0
			;;
		*)
			echo "error: unknown argument: $1" >&2
			usage >&2
			exit 1
			;;
	esac
done

# Detect OS/arch and map to a Rust target triple.
OS="$(uname -s)"
ARCH="$(uname -m)"
case "$OS/$ARCH" in
	Linux/x86_64)
		TARGET="x86_64-unknown-linux-gnu"
		;;
	Linux/aarch64|Linux/arm64)
		TARGET="aarch64-unknown-linux-gnu"
		;;
	Darwin/x86_64)
		TARGET="x86_64-apple-darwin"
		;;
	Darwin/arm64|Darwin/aarch64)
		TARGET="aarch64-apple-darwin"
		;;
	*)
		echo "error: unsupported OS/arch: $OS/$ARCH" >&2
		exit 1
		;;
esac

# Build the download URL for the release asset.
if [[ "$VERSION" == "latest" ]]; then
	URL="https://github.com/${REPO}/releases/latest/download/tokos-${TARGET}.tar.gz"
else
	URL="https://github.com/${REPO}/releases/download/${VERSION}/tokos-${TARGET}.tar.gz"
fi

# Download a URL to a destination path using curl (preferred) or wget.
# Exits non-zero on failure.
download() {
	local url="$1"
	local dest="$2"
	if command -v curl >/dev/null 2>&1; then
		curl --fail --silent --show-error --location --output "$dest" "$url"
	elif command -v wget >/dev/null 2>&1; then
		wget --quiet --output-document="$dest" "$url"
	else
		echo "error: neither curl nor wget is installed" >&2
		return 1
	fi
}

# Pre-flight: ensure a download tool is available.
if ! command -v curl >/dev/null 2>&1 && ! command -v wget >/dev/null 2>&1; then
	echo "error: neither curl nor wget is installed" >&2
	exit 1
fi

TMPDIR="$(mktemp -d)"
trap 'rm -rf "$TMPDIR"' EXIT

ARCHIVE="$TMPDIR/tokos-${TARGET}.tar.gz"
echo "==> Downloading tokos ${VERSION} for ${TARGET}"
if ! download "$URL" "$ARCHIVE"; then
	echo "error: failed to download $URL" >&2
	exit 1
fi

tar -xzf "$ARCHIVE" -C "$TMPDIR"

RELEASE_BIN="$TMPDIR/tokos"
if [[ ! -x "$RELEASE_BIN" ]]; then
	echo "error: extracted binary not found or not executable at $RELEASE_BIN" >&2
	exit 1
fi

# Install the release binary to a destination path, using sudo only if needed.
# Returns non-zero if the destination is not writable and sudo is unavailable.
install_to() {
	local dest="$1"
	local dest_dir
	dest_dir="$(dirname "$dest")"
	mkdir -p "$dest_dir" 2>/dev/null || true
	if [[ -w "$dest_dir" ]]; then
		install -m 0755 "$RELEASE_BIN" "$dest"
	elif command -v sudo >/dev/null 2>&1; then
		sudo install -m 0755 "$RELEASE_BIN" "$dest"
	else
		return 1
	fi
}

DEFAULT_PREFIX=1
if [[ -n "$TO_DIR" ]]; then
	BINDIR="$TO_DIR"
	DEFAULT_PREFIX=0
else
	BINDIR="$PREFIX/bin"
	if [[ "$PREFIX" != "/usr/local" ]]; then
		DEFAULT_PREFIX=0
	fi
fi

DEST="$BINDIR/tokos"
if ! install_to "$DEST"; then
	if [[ $DEFAULT_PREFIX -eq 1 ]]; then
		echo "==> Cannot write to $BINDIR; falling back to $HOME/.local/bin"
		BINDIR="$HOME/.local/bin"
		mkdir -p "$BINDIR"
		DEST="$BINDIR/tokos"
		install -m 0755 "$RELEASE_BIN" "$DEST"
	else
		echo "error: cannot write to $BINDIR" >&2
		exit 1
	fi
fi

echo "==> Installed tokos to $DEST"

case ":$PATH:" in
	*":$BINDIR:"*)
		;;
	*)
		echo "==> Add $BINDIR to your PATH:"
		echo "    export PATH=\"$BINDIR:\$PATH\""
		;;
esac

if version_out="$("$DEST" --version 2>/dev/null)"; then
	echo "==> $version_out"
fi

exit 0
