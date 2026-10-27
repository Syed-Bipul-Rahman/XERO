# Installing Xero

## 1. Get `xeroc`

Download the archive for your platform from the
[Releases page](https://github.com/Syed-Bipul-Rahman/XERO/releases), extract it,
and put `xeroc` (`xeroc.exe` on Windows) on your `PATH`.

| Platform | Archive |
|---|---|
| Linux x86-64 | `xeroc-linux-x86_64.tar.gz` |
| Linux ARM64 | `xeroc-linux-aarch64.tar.gz` |
| macOS Apple Silicon | `xeroc-macos-arm64.tar.gz` |
| macOS Intel | `xeroc-macos-x86_64.tar.gz` |
| Windows x86-64 | `xeroc-windows-x86_64.zip` |

Or build it from source with a Rust toolchain: `cargo build --release`.

## 2. Install clang (the LLVM back end)

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
