#!/bin/sh
# cthread installer for macOS and Linux.
#
#   curl -fsSL https://raw.githubusercontent.com/IqbalLx/cthread/main/install.sh | bash
#
# Environment variables:
#   CTHREAD_VERSION      release tag to install, e.g. v0.1.0 (default: latest)
#   CTHREAD_INSTALL_DIR  where to put the binary (default: ~/.local/bin)
#   CTHREAD_FROM_SOURCE  set to 1 to build with cargo instead of downloading

set -eu

REPO="IqbalLx/cthread"
BASE_URL="${CTHREAD_BASE_URL:-https://github.com/$REPO/releases}"

say() { printf '\033[1mcthread:\033[0m %s\n' "$*" >&2; }
warn() { printf '\033[1;33mcthread:\033[0m %s\n' "$*" >&2; }
die() { printf '\033[1;31mcthread:\033[0m %s\n' "$*" >&2; exit 1; }
has() { command -v "$1" >/dev/null 2>&1; }

fetch() { # fetch <url> <output-file>
    if has curl; then
        curl -fsSL --retry 3 -o "$2" "$1"
    elif has wget; then
        wget -q -O "$2" "$1"
    else
        die "need curl or wget to download cthread"
    fi
}

detect_target() {
    os=$(uname -s)
    arch=$(uname -m)
    case "$arch" in
        x86_64 | amd64) arch=x86_64 ;;
        arm64 | aarch64) arch=aarch64 ;;
        *) return 1 ;;
    esac
    case "$os" in
        Darwin)
            # An x86_64 shell under Rosetta still gets the native arm64 build.
            if [ "$arch" = x86_64 ] && [ "$(sysctl -n sysctl.proc_translated 2>/dev/null)" = 1 ]; then
                arch=aarch64
            fi
            echo "$arch-apple-darwin"
            ;;
        Linux) echo "$arch-unknown-linux-musl" ;;
        *) return 1 ;;
    esac
}

sha256_of() {
    if has sha256sum; then
        sha256sum "$1" | cut -d' ' -f1
    elif has shasum; then
        shasum -a 256 "$1" | cut -d' ' -f1
    else
        echo ""
    fi
}

install_from_source() {
    has cargo || die "no prebuilt binary available and cargo is not installed. Install Rust (https://rustup.rs) and re-run."
    say "building from source with cargo (this takes a minute)"
    if [ -n "${CTHREAD_VERSION:-}" ]; then
        set -- --tag "$CTHREAD_VERSION"
    else
        set --
    fi
    root=$(mktemp -d)
    trap 'rm -rf "$root"' EXIT INT TERM
    cargo install --locked --git "https://github.com/$REPO" "$@" --root "$root" cthread
    mkdir -p "$INSTALL_DIR"
    mv -f "$root/bin/cthread" "$INSTALL_DIR/cthread"
}

install_binary() { # install_binary <target>
    target=$1
    asset="cthread-$target.tar.gz"
    if [ -n "${CTHREAD_VERSION:-}" ]; then
        url="$BASE_URL/download/$CTHREAD_VERSION/$asset"
    else
        url="$BASE_URL/latest/download/$asset"
    fi

    tmp=$(mktemp -d)
    trap 'rm -rf "$tmp"' EXIT INT TERM

    say "downloading $asset (${CTHREAD_VERSION:-latest})"
    fetch "$url" "$tmp/$asset" || return 1

    if fetch "$url.sha256" "$tmp/$asset.sha256" 2>/dev/null; then
        want=$(cut -d' ' -f1 <"$tmp/$asset.sha256")
        got=$(sha256_of "$tmp/$asset")
        if [ -z "$got" ]; then
            warn "no sha256sum or shasum found; skipping checksum check"
        elif [ "$want" != "$got" ]; then
            die "checksum mismatch for $asset (expected $want, got $got)"
        fi
    else
        warn "no checksum published for $asset; skipping checksum check"
    fi

    # This runs inside `if`, where `set -e` is off, so check each step.
    tar -xzf "$tmp/$asset" -C "$tmp" || die "could not unpack $asset"
    mkdir -p "$INSTALL_DIR" || die "could not create $INSTALL_DIR"
    # Install via a temp name + rename so a running cthread isn't clobbered mid-write.
    if ! { cp "$tmp/cthread" "$INSTALL_DIR/.cthread.new" &&
        chmod 755 "$INSTALL_DIR/.cthread.new" &&
        mv -f "$INSTALL_DIR/.cthread.new" "$INSTALL_DIR/cthread"; }; then
        die "could not write $INSTALL_DIR/cthread"
    fi
}

main() {
    INSTALL_DIR="${CTHREAD_INSTALL_DIR:-$HOME/.local/bin}"

    if [ "${CTHREAD_FROM_SOURCE:-0}" = 1 ]; then
        install_from_source
    elif target=$(detect_target); then
        if ! install_binary "$target"; then
            warn "could not download a prebuilt binary for $target"
            install_from_source
        fi
    else
        warn "no prebuilt binary for $(uname -s) $(uname -m)"
        install_from_source
    fi

    say "installed $("$INSTALL_DIR/cthread" --version 2>/dev/null || echo cthread) to $INSTALL_DIR/cthread"

    case ":$PATH:" in
        *":$INSTALL_DIR:"*) ;;
        *)
            warn "$INSTALL_DIR is not on your PATH. Add this to your shell profile:"
            # shellcheck disable=SC2016 # $PATH is meant to be printed literally
            printf '\n    export PATH="%s:$PATH"\n\n' "$INSTALL_DIR" >&2
            ;;
    esac

    if ! has claude; then
        warn "Claude Code ('claude') was not found on your PATH. Install it first: https://claude.com/claude-code"
        warn "(or point cthread at another command with [claude] command in ~/.config/cthread/config.toml)"
    fi

    say "run 'cthread' in a project folder, or 'cthread doctor' to check your setup"
}

# Everything runs from main, so a partially downloaded script does nothing.
main "$@"
