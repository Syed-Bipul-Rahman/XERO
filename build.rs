//! With the `llvm` feature, link LLVM and lld into xeroc (static libraries
//! found with `llvm-config`; set `XERO_LLVM_CONFIG` to choose one).

fn main() {
    println!("cargo:rerun-if-env-changed=XERO_LLVM_CONFIG");
    println!("cargo:rerun-if-env-changed=XERO_LLVM_EXTRA_LIBS");
    println!("cargo:rerun-if-changed=src/llvm/lld_shim.cpp");
    #[cfg(feature = "llvm")]
    link_llvm();
}

#[cfg(feature = "llvm")]
fn link_llvm() {
    use std::process::Command;
    let config = std::env::var("XERO_LLVM_CONFIG").ok().unwrap_or_else(|| {
        ["llvm-config-18", "llvm-config"]
            .iter()
            .find(|c| {
                Command::new(c)
                    .arg("--version")
                    .output()
                    .is_ok_and(|o| o.status.success())
            })
            .unwrap_or(&"llvm-config")
            .to_string()
    });
    let run = |args: &[&str]| -> String {
        let out = Command::new(&config)
            .args(args)
            .output()
            .unwrap_or_else(|e| panic!("cannot run `{config}`: {e} (set XERO_LLVM_CONFIG)"));
        assert!(out.status.success(), "`{config} {}` failed", args.join(" "));
        String::from_utf8(out.stdout).unwrap().trim().to_string()
    };
    let version = run(&["--version"]);
    let major: u32 = version.split('.').next().unwrap().parse().unwrap();
    assert!(
        major >= 17,
        "xeroc's built-in LLVM needs LLVM 17 or newer, found {version}"
    );
    let libdir = run(&["--libdir"]);
    let incdir = run(&["--includedir"]);
    println!("cargo:rustc-link-search=native={libdir}");

    // lld (a library through the C++ shim), then LLVM itself
    cc::Build::new()
        .cpp(true)
        .file("src/llvm/lld_shim.cpp")
        .include(&incdir)
        .flag_if_supported("-std=c++17")
        .flag_if_supported("/std:c++17")
        .flag_if_supported("-fno-exceptions")
        .flag_if_supported("-fno-rtti")
        .warnings(false)
        .compile("xero_lld_shim");
    for lib in ["lldCOFF", "lldELF", "lldMachO", "lldWasm", "lldCommon"] {
        println!("cargo:rustc-link-lib=static={lib}");
    }
    let components = [
        "core",
        "irreader",
        "passes",
        "target",
        "x86",
        "aarch64",
        "riscv",
        "webassembly",
        "arm",
        "option",
        "lto",
        "objcopy",
        "windowsdriver",
        "windowsmanifest",
        "libdriver",
        "dlltooldriver",
    ];
    let mut args = vec!["--link-static", "--libs"];
    args.extend(components);
    for lib in run(&args).split_whitespace() {
        let name = lib
            .trim_start_matches("-l")
            .trim_end_matches(".lib")
            .rsplit(['/', '\\'])
            .next()
            .unwrap()
            .trim_start_matches("lib")
            .trim_end_matches(".a");
        println!("cargo:rustc-link-lib=static={name}");
    }
    for lib in run(&["--link-static", "--system-libs"]).split_whitespace() {
        let name = lib
            .trim_start_matches("-l")
            .trim_end_matches(".lib")
            .rsplit(['/', '\\'])
            .next()
            .unwrap()
            .trim_start_matches("lib")
            .trim_end_matches(".a")
            .trim_end_matches(".so")
            .trim_end_matches(".tbd");
        if !name.is_empty() {
            println!("cargo:rustc-link-lib={name}");
        }
    }
    // libraries a static build also needs (e.g. `static=lzma static=gcc`)
    if let Ok(extra) = std::env::var("XERO_LLVM_EXTRA_LIBS") {
        for lib in extra.split_whitespace() {
            println!("cargo:rustc-link-lib={lib}");
        }
    }
    println!("cargo:rustc-cfg=xero_llvm");
}
