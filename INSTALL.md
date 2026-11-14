# Installing Xero

## One-command install

**Linux / macOS**

```sh
curl -fsSL https://download.xerolang.org/install.sh | sudo bash
```

Without `sudo` it installs to `~/.xero` instead of `/usr/local`.

**Windows (PowerShell)**

```powershell
irm https://download.xerolang.org/install.ps1 | iex
```

The installer:

- picks the archive for your OS and CPU from the latest GitHub release;
- verifies it against the release's `SHA256SUMS`;
- installs `xeroc` and puts it on your `PATH`;
- installs clang if it is missing. Linux uses apt, dnf, yum, pacman, zypper or apk; macOS uses Homebrew or the Xcode Command Line Tools; Windows uses winget. On Windows, building executables also needs the Visual Studio C++ Build Tools, and the installer prints the command to install them if they're missing.

The same scripts are attached to every release, so this also works:
`curl -fsSL https://github.com/Syed-Bipul-Rahman/XERO/releases/latest/download/install.sh | sudo bash`

| Option | Unix (`bash -s -- <option>`) | Windows (environment variable) |
|---|---|---|
| Specific version | `--version v0.1.0` | `$env:XERO_VERSION="v0.1.0"` |
| Install location | `--prefix /opt/xero` | `$env:XERO_INSTALL_DIR="C:\xero"` |
| Skip clang | `--no-deps` | `$env:XERO_NO_DEPS="1"` |
| Uninstall | `--uninstall` | `$env:XERO_UNINSTALL="1"` |

## Manual install

### 1. Get `xeroc`

Download the archive for your platform from the
[Releases page](https://github.com/Syed-Bipul-Rahman/XERO/releases), extract it,
and put `xeroc` (`xeroc.exe` on Windows) on your `PATH`.

| Platform | Archive |
|---|---|
| Linux x86-64 (static) | `xeroc-linux-x86_64.tar.gz` |
| Linux ARM64 (static) | `xeroc-linux-aarch64.tar.gz` |
| macOS Apple Silicon | `xeroc-macos-arm64.tar.gz` |
| macOS Intel | `xeroc-macos-x86_64.tar.gz` |
| Windows x86-64 | `xeroc-windows-x86_64.zip` |

Or build it from source with a Rust toolchain: `cargo build --release`.

### 2. What else is needed

The release builds of `xeroc` have LLVM (and the lld linker) built in:

| OS | Needs |
|---|---|
| Linux | nothing. Programs that call C functions (or use `--libc`) are linked with the system's `cc` (gcc or clang). |
| macOS (Apple Silicon) | the Xcode Command Line Tools (`xcode-select --install`), for Apple's linker and SDK |
| macOS (Intel) | clang: the Xcode Command Line Tools provide it |
| Windows | the Visual Studio C++ Build Tools (linker and C runtime) |

`xeroc --version` says `(LLVM built in)` for such a build.

A `xeroc` built from source with plain `cargo build --release` uses `clang`
(LLVM 15 or newer) instead: `sudo apt install clang lld`, `sudo dnf install
clang lld`, `xcode-select --install`, or `winget install LLVM.LLVM`. Point
`XERO_CLANG` at it if it isn't on your `PATH` as `clang` (setting `XERO_CLANG`
also makes a built-in-LLVM `xeroc` use clang). To build `xeroc` with LLVM
inside, install LLVM 17+ development libraries with lld (for example
`sudo apt install llvm-18-dev liblld-18-dev libpolly-18-dev`) and run
`cargo build --release --features llvm`; `XERO_LLVM_CONFIG` selects the
`llvm-config` to use.

Xero programs don't use the C library on Linux, so cross-compiling for
another architecture (`--target aarch64-linux`, `--target riscv64-linux`)
needs no cross toolchain or sysroot.

## 3. Try it

```console
$ xeroc run examples/hello.xr
Hello, Xero!
$ xeroc run examples/tour.xr
$ xeroc build examples/sieve.xr -O3 -o sieve && ./sieve
$ xeroc build examples/wasm_math.xr --target wasm32 --lib -o math.wasm
```

Run `xeroc --help` for every option and `xeroc targets` for the cross-compilation targets.
