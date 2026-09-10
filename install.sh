#!/bin/sh
# Rocker installer — downloads the latest release, verifies it, and runs
# `rocker install` so you get the binary on PATH plus a real app-menu entry,
# icon, and docker:// URL handler. Works on Linux, any distro/desktop.
#
#   curl --proto '=https' --tlsv1.2 -fsSL https://raw.githubusercontent.com/makis-san/rocker/main/install.sh | sh
#
# Options (after `| sh -s --`): --tag vX.Y.Z   pin a version
#                               --modify-path  add ~/.local/bin to your shell PATH
#                               --system       install into /usr/local (needs sudo)
set -eu

REPO="makis-san/rocker"
# minisign public key that signs SHA256SUMS (base64 blob from rocker.pub). The
# checksum is always verified; the signature is verified too when `minisign` is
# installed, and skipped with a note otherwise (the binary's own `rocker
# self-update` always verifies it).
ROCKER_MINISIGN_PUBKEY="RWTfmkhmM6bfkPa36B5q/LZZ4LEY5tVCqAO5t5fkiGbOBp5ztbkwc3VE"

TAG=""
FORWARD=""
while [ $# -gt 0 ]; do
	case "$1" in
		--tag) TAG="${2:?--tag needs a version}"; shift 2 ;;
		--tag=*) TAG="${1#--tag=}"; shift ;;
		-h|--help) sed -n '2,12p' "$0"; exit 0 ;;
		*) FORWARD="$FORWARD $1"; shift ;;
	esac
done

say() { printf 'rocker-install: %s\n' "$1" >&2; }
die() { say "error: $1"; exit 1; }
have() { command -v "$1" >/dev/null 2>&1; }

# --- pick a downloader -------------------------------------------------------
if have curl; then
	dl() { curl --proto '=https' --tlsv1.2 -fsSL "$1" -o "$2"; }
	dl_stdout() { curl --proto '=https' --tlsv1.2 -fsSL "$1"; }
elif have wget; then
	dl() { wget -qO "$2" "$1"; }
	dl_stdout() { wget -qO- "$1"; }
else
	die "need curl or wget"
fi

# --- target triple ---------------------------------------------------------
os="$(uname -s)"; arch="$(uname -m)"
case "$os" in
	Linux)  vendor_os="unknown-linux-gnu"; ext="tar.xz" ;;
	Darwin) die "macOS isn't packaged yet — build from source (see the README) or use Homebrew" ;;
	*) die "unsupported OS: $os (Windows: use install.ps1)" ;;
esac
case "$arch" in
	x86_64|amd64) cpu="x86_64" ;;
	arm64|aarch64) cpu="aarch64" ;;
	*) die "unsupported architecture: $arch" ;;
esac
TRIPLE="${cpu}-${vendor_os}"

# --- resolve the release tag --------------------------------------------------
if [ -z "$TAG" ]; then
	say "resolving latest release..."
	TAG="$(dl_stdout "https://api.github.com/repos/${REPO}/releases/latest" \
		| grep '"tag_name"' | head -1 | sed 's/.*"tag_name": *"\([^"]*\)".*/\1/')"
	[ -n "$TAG" ] || die "couldn't determine the latest release tag"
fi
say "installing $TAG for $TRIPLE"

BASE="https://github.com/${REPO}/releases/download/${TAG}"
ARCHIVE="rocker-${TRIPLE}.${ext}"

TMP="$(mktemp -d "${TMPDIR:-/tmp}/rocker-install.XXXXXX")"
trap 'rm -rf "$TMP"' EXIT INT TERM

say "downloading $ARCHIVE"
dl "${BASE}/${ARCHIVE}" "${TMP}/${ARCHIVE}"        || die "download failed: ${BASE}/${ARCHIVE}"
dl "${BASE}/SHA256SUMS" "${TMP}/SHA256SUMS"        || die "download failed: SHA256SUMS"
dl "${BASE}/SHA256SUMS.minisig" "${TMP}/SHA256SUMS.minisig" 2>/dev/null || true

# --- verify checksum -------------------------------------------------------
if have sha256sum; then sha_cmd="sha256sum";
elif have shasum;   then sha_cmd="shasum -a 256";
else die "need sha256sum or shasum to verify the download"; fi

want="$(grep " \*\{0,1\}${ARCHIVE}\$" "${TMP}/SHA256SUMS" | awk '{print $1}')"
[ -n "$want" ] || die "$ARCHIVE not listed in SHA256SUMS"
got="$(cd "$TMP" && $sha_cmd "$ARCHIVE" | awk '{print $1}')"
[ "$want" = "$got" ] || die "checksum mismatch for $ARCHIVE (expected $want, got $got)"
say "checksum ok"

# --- verify signature -----------------------------------------------------
# The checksum above is already fetched over HTTPS from GitHub. A minisign
# signature closes the supply-chain gap; verify it when the tool is present.
if [ -z "$ROCKER_MINISIGN_PUBKEY" ]; then
	say "note: no signing key in this installer; relying on the HTTPS checksum"
elif [ ! -s "${TMP}/SHA256SUMS.minisig" ]; then
	say "note: this release has no SHA256SUMS.minisig; relying on the HTTPS checksum"
elif have minisign; then
	printf '%s\n' "$ROCKER_MINISIGN_PUBKEY" > "${TMP}/rocker.pub"
	minisign -Vm "${TMP}/SHA256SUMS" -p "${TMP}/rocker.pub" >/dev/null \
		|| die "minisign signature verification failed"
	say "signature ok"
else
	say "note: minisign not installed; skipping signature check (checksum verified"
	say "      over HTTPS). \`brew/apt install minisign\` for full verification."
fi

# --- unpack and hand off to the binary -----------------------------------
say "unpacking"
tar -xf "${TMP}/${ARCHIVE}" -C "$TMP"
BIN="$(find "$TMP" -type f -name rocker -perm -u+x | head -1)"
[ -n "$BIN" ] || BIN="$(find "$TMP" -type f -name rocker | head -1)"
[ -n "$BIN" ] || die "archive did not contain the rocker binary"
chmod +x "$BIN"

say "running: rocker install$FORWARD"
# shellcheck disable=SC2086
"$BIN" install $FORWARD

say "done. launch Rocker from your application menu, or run: rocker"
