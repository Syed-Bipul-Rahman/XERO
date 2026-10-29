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

### 2. Install clang (the LLVM back end)

`xeroc` generates LLVM IR and uses `clang` (LLVM 17 or newer) to turn it
into machine code and link it.

| OS | Command |
|---|---|
| Ubuntu/Debian | `sudo apt install clang lld` |
| Fedora | `sudo dnf install clang lld` |
| macOS | `xcode-select --install` (Apple clang) or `brew install llvm` |
| Windows | `winget install LLVM.LLVM`, plus the Visual Studio Build Tools (C++ workload) for the linker and C runtime |

If clang isn't on your `PATH` under the name `clang`, point `XERO_CLANG` at it,
for example `XERO_CLANG=/opt/homebrew/opt/llvm/bin/clang`.

## 3. Try it

```console
$ xeroc run examples/hello.xr
Hello, Xero!
$ xeroc run examples/tour.xr
$ xeroc build examples/sieve.xr -O3 -o sieve && ./sieve
$ xeroc build examples/wasm_math.xr --target wasm32 --lib -o math.wasm
```

Run `xeroc --help` for every option and `xeroc targets` for the cross-compilation targets.
