use std::path::PathBuf;
use std::process::{exit, Command};

use xeroc::driver::{build, Emit, Options};

const USAGE: &str = "\
xeroc — the Xero compiler

USAGE:
    xeroc build <file.xr> [options]     compile to a native executable (or --emit)
    xeroc run <file.xr> [-- args...]    compile and run
    xeroc check <file.xr>               type- and borrow-check only
    xeroc targets                       list supported targets

OPTIONS:
    -o <path>            output path
    --emit <kind>        exe | obj | asm | llvm | llvm-raw | xir   (default: exe)
    -O0 | -O1 | -O2 | -O3 | -Os
                         optimization level (default: -O2)
    --target <name>      cross-compile (see `xeroc targets`)
    --lib                no `main` required; build a library object
    --att                AT&T assembly syntax on x86 (default: Intel)
";

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() || args[0] == "-h" || args[0] == "--help" || args[0] == "help" {
        print!("{USAGE}");
        return;
    }
    if args[0] == "--version" || args[0] == "-V" {
        println!("xeroc {}", env!("CARGO_PKG_VERSION"));
        return;
    }
    let cmd = args[0].as_str();
    if cmd == "targets" {
        for t in xeroc::target::TARGETS {
            println!("{:<16} {}", t.name, t.triple);
        }
        return;
    }
    if !matches!(cmd, "build" | "run" | "check") {
        eprintln!("error: unknown command `{cmd}`\n\n{USAGE}");
        exit(2);
    }
    let mut input = None;
    let mut output = None;
    let mut emit = Emit::Exe;
    let mut opt_level = 2;
    let mut target = xeroc::target::host();
    let mut lib = false;
    let mut intel = true;
    let mut run_args = Vec::new();
    let mut i = 1;
    while i < args.len() {
        let a = &args[i];
        let value = |i: &mut usize| -> String {
            *i += 1;
            match args.get(*i) {
                Some(v) => v.clone(),
                None => {
                    eprintln!("error: `{a}` expects a value");
                    exit(2);
                }
            }
        };
        match a.as_str() {
            "--" => {
                run_args = args[i + 1..].to_vec();
                break;
            }
            "-o" => output = Some(PathBuf::from(value(&mut i))),
            "--emit" => {
                emit = match value(&mut i).as_str() {
                    "exe" => Emit::Exe,
                    "obj" => Emit::Obj,
                    "asm" => Emit::Asm,
                    "llvm" => Emit::Llvm,
                    "llvm-raw" => Emit::LlvmRaw,
                    "xir" => Emit::Xir,
                    other => {
                        eprintln!("error: unknown emit kind `{other}`");
                        exit(2);
                    }
                }
            }
            "--target" => {
                let t = value(&mut i);
                target = match xeroc::target::find(&t) {
                    Some(t) => t,
                    None => {
                        eprintln!("error: unknown target `{t}` (see `xeroc targets`)");
                        exit(2);
                    }
                }
            }
            "-O0" => opt_level = 0,
            "-O1" => opt_level = 1,
            "-O2" => opt_level = 2,
            "-O3" => opt_level = 3,
            "-Os" => opt_level = 2,
            "--lib" => lib = true,
            "--att" => intel = false,
            s if s.starts_with("--emit=") => {
                args_emit(&s[7..], &mut emit);
            }
            s if s.starts_with("--target=") => {
                target = xeroc::target::find(&s[9..]).unwrap_or_else(|| {
                    eprintln!("error: unknown target `{}`", &s[9..]);
                    exit(2);
                })
            }
            s if s.starts_with('-') => {
                eprintln!("error: unknown option `{s}`");
                exit(2);
            }
            s => {
                if input.is_some() {
                    eprintln!("error: multiple input files");
                    exit(2);
                }
                input = Some(PathBuf::from(s));
            }
        }
        i += 1;
    }
    let Some(input) = input else {
        eprintln!("error: no input file\n\n{USAGE}");
        exit(2);
    };
    if cmd == "check" {
        let opts = Options {
            input,
            output: None,
            emit: Emit::Xir,
            opt_level,
            target,
            lib,
            intel_syntax: intel,
        };
        let mut fe = xeroc::driver::Frontend {
            sm: Default::default(),
        };
        match xeroc::driver::compile_to_llvm(&mut fe, &opts) {
            Ok(_) => return,
            Err(xeroc::driver::FrontendError::Diags(ds)) => {
                eprint!("{}", fe.render(&ds));
                exit(1);
            }
            Err(xeroc::driver::FrontendError::Io(e)) => {
                eprintln!("error: {e}");
                exit(1);
            }
        }
    }
    let run = cmd == "run";
    let output = if run && output.is_none() {
        let stem = input.file_stem().and_then(|s| s.to_str()).unwrap_or("prog");
        Some(
            std::env::temp_dir()
                .join(format!("xero-run-{}-{stem}", std::process::id()))
                .with_extension(std::env::consts::EXE_EXTENSION),
        )
    } else {
        output
    };
    let opts = Options {
        input,
        output,
        emit: if run { Emit::Exe } else { emit },
        opt_level,
        target,
        lib,
        intel_syntax: intel,
    };
    match build(&opts) {
        Ok(path) => {
            if run {
                let status = Command::new(&path).args(&run_args).status();
                let _ = std::fs::remove_file(&path);
                match status {
                    Ok(s) => exit(s.code().unwrap_or(1)),
                    Err(e) => {
                        eprintln!("error: failed to run `{}`: {e}", path.display());
                        exit(1);
                    }
                }
            }
        }
        Err(msg) => {
            eprint!("{msg}");
            exit(1);
        }
    }
}

fn args_emit(s: &str, emit: &mut Emit) {
    *emit = match s {
        "exe" => Emit::Exe,
        "obj" => Emit::Obj,
        "asm" => Emit::Asm,
        "llvm" => Emit::Llvm,
        "llvm-raw" => Emit::LlvmRaw,
        "xir" => Emit::Xir,
        other => {
            eprintln!("error: unknown emit kind `{other}`");
            exit(2);
        }
    }
}
