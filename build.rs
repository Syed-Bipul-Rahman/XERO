//! With the `llvm` feature, link LLVM and lld into xeroc (static libraries
//! found with `llvm-config`; set `XERO_LLVM_CONFIG` to choose one).

fn main() {
    println!("cargo:rerun-if-env-changed=XERO_LLVM_CONFIG");
    println!("cargo:rerun-if-env-changed=XERO_LLVM_EXTRA_LIBS");
    println!("cargo:rerun-if-env-changed=XERO_LLVM_STATIC_SYSLIBS");
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
    // link against a directory holding only LLVM's static libraries, so the
    // linker never picks up the LLVM package's own shared C++ runtime
    // (macOS: its libc++.dylib would shadow the system one)
    let search = static_lib_dir(&libdir);
    println!("cargo:rustc-link-search=native={search}");

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
        let base = lib
            .trim_start_matches("-l")
            .rsplit(['/', '\\'])
            .next()
            .unwrap();
        let name = if base.ends_with(".lib") {
            base.trim_end_matches(".lib")
        } else {
            base.trim_start_matches("lib")
                .trim_end_matches(".a")
                .trim_end_matches(".so")
                .trim_end_matches(".tbd")
        };
        // libxml2 is only used by the Windows manifest merger, which the
        // shim replaces
        if name.contains("xml2") {
            continue;
        }
        // system libraries to link statically, e.g. Homebrew's zstd on macOS
        let static_sys = std::env::var("XERO_LLVM_STATIC_SYSLIBS").unwrap_or_default();
        if static_sys.split_whitespace().any(|s| s == name) {
            println!("cargo:rustc-link-lib=static={name}");
        } else if !name.is_empty() {
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

#[cfg(feature = "llvm")]
fn static_lib_dir(libdir: &str) -> String {
    #[cfg(unix)]
    {
        let out = std::path::PathBuf::from(std::env::var("OUT_DIR").unwrap()).join("llvm-static");
        let _ = std::fs::remove_dir_all(&out);
        std::fs::create_dir_all(&out).unwrap();
        for e in std::fs::read_dir(libdir).unwrap().flatten() {
            let p = e.path();
            let name = e.file_name().to_string_lossy().into_owned();
            // the system's C++ runtime is used, not the package's
            let runtime = name.starts_with("libc++") || name.starts_with("libunwind");
            if p.extension().is_some_and(|x| x == "a") && !runtime {
                std::os::unix::fs::symlink(&p, out.join(e.file_name())).unwrap();
            }
        }
        out.display().to_string()
    }
    #[cfg(not(unix))]
    {
        libdir.to_string()
    }
}
