//! Compilation targets.

#[derive(Clone, Debug)]
pub struct TargetInfo {
    /// Name accepted on the command line.
    pub name: &'static str,
    pub triple: &'static str,
    pub ptr_bits: u32,
    /// Can we link and run executables for this target on this host?
    pub hosted: bool,
}

pub const TARGETS: &[TargetInfo] = &[
    TargetInfo {
        name: "x86_64-linux",
        triple: "x86_64-unknown-linux-gnu",
        ptr_bits: 64,
        hosted: true,
    },
    TargetInfo {
        name: "aarch64-linux",
        triple: "aarch64-unknown-linux-gnu",
        ptr_bits: 64,
        hosted: true,
    },
    TargetInfo {
        name: "riscv64-linux",
        triple: "riscv64-unknown-linux-gnu",
        ptr_bits: 64,
        hosted: true,
    },
    TargetInfo {
        name: "x86_64-macos",
        triple: "x86_64-apple-macosx11.0.0",
        ptr_bits: 64,
        hosted: true,
    },
    TargetInfo {
        name: "aarch64-macos",
        triple: "arm64-apple-macosx11.0.0",
        ptr_bits: 64,
        hosted: true,
    },
    TargetInfo {
        name: "x86_64-windows",
        triple: "x86_64-pc-windows-msvc",
        ptr_bits: 64,
        hosted: true,
    },
    TargetInfo {
        name: "wasm32",
        triple: "wasm32-unknown-unknown",
        ptr_bits: 32,
        hosted: false,
    },
    TargetInfo {
        name: "x86_64-none",
        triple: "x86_64-unknown-none-elf",
        ptr_bits: 64,
        hosted: false,
    },
    TargetInfo {
        name: "aarch64-none",
        triple: "aarch64-unknown-none-elf",
        ptr_bits: 64,
        hosted: false,
    },
    TargetInfo {
        name: "riscv64-none",
        triple: "riscv64-unknown-none-elf",
        ptr_bits: 64,
        hosted: false,
    },
    TargetInfo {
        name: "riscv32-none",
        triple: "riscv32-unknown-none-elf",
        ptr_bits: 32,
        hosted: false,
    },
    TargetInfo {
        name: "thumbv7em-none",
        triple: "thumbv7em-none-eabi",
        ptr_bits: 32,
        hosted: false,
    },
];

pub fn host() -> TargetInfo {
    let name = match (std::env::consts::ARCH, std::env::consts::OS) {
        ("x86_64", "linux") => "x86_64-linux",
        ("aarch64", "linux") => "aarch64-linux",
        ("riscv64", "linux") => "riscv64-linux",
        ("x86_64", "macos") => "x86_64-macos",
        ("aarch64", "macos") => "aarch64-macos",
        ("x86_64", "windows") => "x86_64-windows",
        _ => "x86_64-linux",
    };
    find(name).unwrap()
}

pub fn find(name: &str) -> Option<TargetInfo> {
    TARGETS
        .iter()
        .find(|t| t.name == name || t.triple == name)
        .cloned()
}
