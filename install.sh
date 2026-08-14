#!/bin/sh
# Install devctx from the latest GitHub release.
#
#   curl -fsSL https://raw.githubusercontent.com/snaven10/DevCtxEngine/main/install.sh | sh
#
# Deliberately small: it downloads one binary and puts it on your PATH. It does
# not write configuration, create projects or fetch models — those are choices,
# and choosing them for you is how a setup ends up in a state nobody can
# explain. `AGENTS.md` walks through them, and is written so a coding agent can
# follow it as well as a person.
set -eu

REPO="${DEVCTX_REPO:-snaven10/DevCtxEngine}"
BIN_DIR="${DEVCTX_BIN_DIR:-$HOME/.local/bin}"

die() { printf 'install.sh: %s\n' "$1" >&2; exit 1; }

need() { command -v "$1" >/dev/null 2>&1 || die "this needs $1"; }
need uname
need tar

if command -v curl >/dev/null 2>&1; then
  fetch() { curl -fsSL "$1"; }
  fetch_to() { curl -fsSL -o "$2" "$1"; }
elif command -v wget >/dev/null 2>&1; then
  fetch() { wget -qO- "$1"; }
  fetch_to() { wget -qO "$2" "$1"; }
else
  die "this needs curl or wget"
fi

os="$(uname -s)"
arch="$(uname -m)"
case "$os-$arch" in
  Linux-x86_64)          target="x86_64-unknown-linux-gnu" ;;
  Darwin-arm64)          target="aarch64-apple-darwin" ;;
  *)
    die "no prebuilt binary for $os-$arch — build from source: cargo build --release" ;;
esac

tag="${DEVCTX_VERSION:-}"
if [ -z "$tag" ]; then
  # The redirect from /releases/latest carries the tag, which avoids needing an
  # API token and avoids parsing JSON in /bin/sh.
  tag="$(fetch "https://api.github.com/repos/$REPO/releases/latest" \
    | sed -n 's/.*"tag_name": *"\([^"]*\)".*/\1/p' | head -n 1)"
  [ -n "$tag" ] || die "could not determine the latest release of $REPO"
fi

name="devctx-$target"
url="https://github.com/$REPO/releases/download/$tag/$name.tar.gz"

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT INT TERM

printf 'Downloading %s %s (%s)…\n' "devctx" "$tag" "$target"
fetch_to "$url" "$tmp/$name.tar.gz" || die "download failed: $url"

# Verify when the checksum is published; a corrupt download that runs is worse
# than one that fails.
if fetch_to "$url.sha256" "$tmp/$name.tar.gz.sha256" 2>/dev/null; then
  if command -v shasum >/dev/null 2>&1; then
    (cd "$tmp" && shasum -a 256 -c "$name.tar.gz.sha256" >/dev/null) || die "checksum mismatch"
  elif command -v sha256sum >/dev/null 2>&1; then
    (cd "$tmp" && sha256sum -c "$name.tar.gz.sha256" >/dev/null) || die "checksum mismatch"
  fi
fi

tar -C "$tmp" -xzf "$tmp/$name.tar.gz"
mkdir -p "$BIN_DIR"
install -m755 "$tmp/$name/devctx" "$BIN_DIR/devctx" 2>/dev/null \
  || { cp "$tmp/$name/devctx" "$BIN_DIR/devctx" && chmod 755 "$BIN_DIR/devctx"; }

printf 'Installed %s\n' "$BIN_DIR/devctx"

case ":$PATH:" in
  *":$BIN_DIR:"*) ;;
  *) printf '\n%s is not on your PATH. Add it:\n    export PATH="%s:$PATH"\n' "$BIN_DIR" "$BIN_DIR" ;;
esac

cat <<'NEXT'

Next: devctx needs an embedding model chosen before anything is indexed, and
changing it later means re-indexing from scratch.

    https://github.com/snaven10/DevCtxEngine/blob/main/AGENTS.md

That file is the setup procedure — including migrating memories from an older
DevAI install — written so a coding agent can carry it out. Point yours at it.
NEXT
