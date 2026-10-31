#!/usr/bin/env bash
# Xero installer for Linux and macOS.
#
#   curl -fsSL https://download.xerolang.org/install.sh | sudo bash
#   curl -fsSL https://download.xerolang.org/install.sh | bash          # per-user, no sudo
#   curl -fsSL https://download.xerolang.org/install.sh | bash -s -- --version v0.1.0
#   curl -fsSL https://download.xerolang.org/install.sh | sudo bash -s -- --uninstall
#
# Options (or environment variables):
#   --version <tag>      XERO_VERSION       release to install (default: latest)
#   --prefix <dir>       XERO_PREFIX        install prefix (default: /usr/local as root, ~/.xero otherwise)
#   --no-deps            XERO_NO_DEPS=1     do not try to install clang
#   --uninstall                             remove xeroc
#   XERO_REPO      GitHub repository to download from (default: Syed-Bipul-Rahman/XERO)
#   XERO_DOWNLOAD_BASE  base URL holding the archives and SHA256SUMS (mirrors)
#   XERO_ARCHIVE   install from a local archive instead of downloading (for testing)

# Everything is inside main() so a partially downloaded script never runs.
main() {
    set -euo pipefail

    local repo="${XERO_REPO:-Syed-Bipul-Rahman/XERO}"
    local version="${XERO_VERSION:-latest}"
    local prefix="${XERO_PREFIX:-}"
    local no_deps="${XERO_NO_DEPS:-0}"
    local uninstall=0

    while [ $# -gt 0 ]; do
        case "$1" in
            --version) version="$2"; shift 2 ;;
            --prefix) prefix="$2"; shift 2 ;;
            --no-deps) no_deps=1; shift ;;
            --uninstall) uninstall=1; shift ;;
            -h|--help) sed -n '2,18p' "$0" 2>/dev/null || true; return 0 ;;
            *) err "unknown option: $1" ;;
        esac
    done

    if [ -z "$prefix" ]; then
        if [ "$(id -u)" = "0" ]; then prefix="/usr/local"; else prefix="$HOME/.xero"; fi
    fi
    local bindir="$prefix/bin"
    local sharedir="$prefix/share/xero"

    if [ "$uninstall" = "1" ]; then
        rm -f "$bindir/xeroc"
        rm -rf "$sharedir"
        say "removed xeroc from $bindir"
        return 0
    fi

    # ---------------------------------------------------------- platform
    local os arch platform ext=tar.gz
    os="$(uname -s)"
    arch="$(uname -m)"
    case "$os" in
        Linux)
            case "$arch" in
                x86_64|amd64) platform=linux-x86_64 ;;
                aarch64|arm64) platform=linux-aarch64 ;;
                *) err "unsupported Linux architecture: $arch (build from source: https://github.com/$repo)" ;;
            esac ;;
        Darwin)
            case "$arch" in
                arm64|aarch64) platform=macos-arm64 ;;
                x86_64) platform=macos-x86_64 ;;
                *) err "unsupported macOS architecture: $arch" ;;
            esac ;;
        *) err "unsupported OS: $os (on Windows use install.ps1)" ;;
    esac
    local asset="xeroc-$platform.$ext"

    # ---------------------------------------------------------- download
    local tmp
    tmp="$(mktemp -d)"
    # expand $tmp now: the trap runs after main's locals are gone
    trap "rm -rf '$tmp'" EXIT

    if [ -n "${XERO_ARCHIVE:-}" ]; then
        say "installing from local archive $XERO_ARCHIVE"
        cp "$XERO_ARCHIVE" "$tmp/$asset"
    else
        local base
        if [ -n "${XERO_DOWNLOAD_BASE:-}" ]; then
            base="$XERO_DOWNLOAD_BASE"
        elif [ "$version" = "latest" ]; then
            base="https://github.com/$repo/releases/latest/download"
        else
            base="https://github.com/$repo/releases/download/$version"
        fi
        say "downloading $asset ($version)"
        download "$base/$asset" "$tmp/$asset" || err "download failed: $base/$asset"
        if download "$base/SHA256SUMS" "$tmp/SHA256SUMS" 2>/dev/null; then
            verify "$tmp" "$asset"
        else
            warn "no SHA256SUMS published for this release; skipping checksum verification"
        fi
    fi

    tar -xzf "$tmp/$asset" -C "$tmp"
    local src="$tmp/xeroc-$platform"
    [ -x "$src/xeroc" ] || err "archive does not contain xeroc"

    # ---------------------------------------------------------- install
    mkdir -p "$bindir" "$sharedir" || err "cannot create $bindir (try: curl ... | sudo bash)"
    install -m 755 "$src/xeroc" "$bindir/xeroc"
    rm -rf "$sharedir/examples" "$sharedir/docs"
    cp -R "$src/examples" "$src/docs" "$sharedir/" 2>/dev/null || true
    cp "$src/README.md" "$src/INSTALL.md" "$sharedir/" 2>/dev/null || true
    if [ "$os" = "Darwin" ]; then
        xattr -d com.apple.quarantine "$bindir/xeroc" 2>/dev/null || true
    fi
    say "installed $("$bindir/xeroc" --version) to $bindir/xeroc"

    # ---------------------------------------------------------- clang
    local cc
    if cc="$(find_clang)"; then
        say "found clang: $(clang_version "$cc")"
    elif [ "$no_deps" = "1" ]; then
        warn "no clang with LLVM $MIN_LLVM+ found; install one before using xeroc"
    else
        install_clang "$os"
    fi

    # ---------------------------------------------------------- PATH
    case ":$PATH:" in
        *":$bindir:"*) ;;
        *)
            warn "$bindir is not on your PATH. Add this to your shell profile:"
            printf '\n    export PATH="%s:$PATH"\n\n' "$bindir" >&2
            ;;
    esac

    printf '\n'
    say "Xero is ready. Try it:"
    printf '    xeroc run %s/examples/hello.xr\n\n' "$sharedir"
}

say() { printf '\033[1;32mxero:\033[0m %s\n' "$*"; }
warn() { printf '\033[1;33mxero:\033[0m %s\n' "$*" >&2; }
err() { printf '\033[1;31mxero: error:\033[0m %s\n' "$*" >&2; exit 1; }
have() { command -v "$1" >/dev/null 2>&1; }

download() {
    if have curl; then
        curl -fsSL --retry 3 -o "$2" "$1"
    elif have wget; then
        wget -q -O "$2" "$1"
    else
        err "need curl or wget"
    fi
}

verify() {
    local dir="$1" asset="$2" want got
    want="$(grep " $asset\$" "$dir/SHA256SUMS" | awk '{print $1}')"
    [ -n "$want" ] || { warn "$asset not listed in SHA256SUMS"; return 0; }
    if have sha256sum; then
        got="$(sha256sum "$dir/$asset" | awk '{print $1}')"
    elif have shasum; then
        got="$(shasum -a 256 "$dir/$asset" | awk '{print $1}')"
    else
        warn "no sha256 tool found; skipping checksum verification"
        return 0
    fi
    [ "$want" = "$got" ] || err "checksum mismatch for $asset (expected $want, got $got)"
    say "checksum verified"
}

# xeroc emits LLVM IR that needs LLVM 15+ (opaque pointers).
MIN_LLVM=15

clang_version() {
    "$1" --version 2>/dev/null | head -n 1
}

clang_major() {
    "$1" --version 2>/dev/null | sed -n 's/.*clang version \([0-9][0-9]*\).*/\1/p' | head -n 1
}

# Print the first clang that is new enough: $XERO_CLANG, clang, clang-24 ... clang-15.
# (xeroc searches the same names, so a versioned clang-18 is found without setup.)
find_clang() {
    local c v
    for c in ${XERO_CLANG:-} clang $(seq 24 -1 "$MIN_LLVM" | sed 's/^/clang-/'); do
        have "$c" || continue
        v="$(clang_major "$c")"
        [ -n "$v" ] || continue
        if [ "$v" -ge "$MIN_LLVM" ]; then
            echo "$c"
            return 0
        fi
        # Apple clang numbers its releases differently; any current Xcode works
        if [ "$(uname -s)" = "Darwin" ] && [ "$v" -ge 14 ]; then
            echo "$c"
            return 0
        fi
    done
    return 1
}

as_root() {
    if [ "$(id -u)" = "0" ]; then
        "$@"
    elif have sudo; then
        sudo "$@"
    else
        return 1
    fi
}

install_clang() {
    local os="$1" v
    if have clang; then
        say "$(clang_version clang) is too old (Xero needs LLVM $MIN_LLVM+); installing a newer clang"
    else
        say "clang (the LLVM back end Xero uses for code generation) was not found; installing it"
    fi
    if [ "$os" = "Darwin" ]; then
        if have brew; then
            brew install llvm && {
                warn "Homebrew's clang is not on PATH by default; add: export XERO_CLANG=\"$(brew --prefix llvm)/bin/clang\""
                return 0
            }
        fi
        warn "install the Xcode Command Line Tools (provides clang):"
        printf '\n    xcode-select --install\n\n' >&2
        xcode-select --install 2>/dev/null || true
        return 0
    fi
    # The distribution's default clang first; if that is still too old (for
    # example clang 14 on Ubuntu 22.04 or Debian 12), the newest versioned package.
    if have apt-get; then
        as_root apt-get update -qq || true
        find_clang >/dev/null || as_root apt-get install -y -qq clang lld >/dev/null 2>&1 || true
        for v in 20 19 18 17 16 15; do
            find_clang >/dev/null && break
            as_root apt-get install -y -qq "clang-$v" "lld-$v" >/dev/null 2>&1 || true
        done
    elif have dnf; then
        as_root dnf install -y clang lld || true
    elif have yum; then
        as_root yum install -y clang || true
    elif have pacman; then
        as_root pacman -Sy --noconfirm clang lld || true
    elif have zypper; then
        as_root zypper --non-interactive install clang lld || true
    elif have apk; then
        as_root apk add clang lld musl-dev || true
    fi
    local cc
    if cc="$(find_clang)"; then
        say "installed $(clang_version "$cc")"
    else
        warn "could not install clang $MIN_LLVM+ automatically; install it with your package manager"
        warn "(on older distributions, https://apt.llvm.org provides current versions)"
    fi
}

main "$@"
