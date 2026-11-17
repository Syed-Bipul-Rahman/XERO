use std::path::PathBuf;
use std::process::{exit, Command};

use xeroc::driver::{build, Emit, Libc, Options};

const USAGE: &str = "\
xeroc — the Xero compiler

USAGE:
    xeroc build <file.xr> [options]     compile to a native executable (or --emit)
    xeroc run <file.xr> [-- args...]    compile and run
    xeroc check <file.xr>               type- and borrow-check only
    xeroc targets                       list supported targets

PROJECTS (a directory with a xero.toml):
    xeroc new <name> [--lib]            create a project
    xeroc build                         build the project into build/<name>
    xeroc run [-- args...]              build and run it
    xeroc check                         check it
    xeroc test [filter]                 run the fn test_...() functions in tests/*.xr
    xeroc fmt [files...] [--check]      format source files (in a project: src/ and tests/)

OPTIONS:
    -o <path>            output path
    --emit <kind>        exe | obj | asm | llvm | llvm-raw | xir   (default: exe)
    -O0 | -O1 | -O2 | -O3 | -Os
                         optimization level (default: -O2)
    -g                   debug information, for gdb and lldb (best with -O0)
    --overflow-checks    panic when +, - or * on integers overflows
                         (the default below -O2)
    --no-overflow-checks wrap around instead (the default at -O2 and up)
    --target <name>      cross-compile (see `xeroc targets`)
    --lib                no `main` required; build a library object
    --libc               link the C library (Linux; automatic when the
                         program declares C functions)
    --no-libc            never link the C library (Linux)
    --att                AT&T assembly syntax on x86 (default: Intel)
";

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() || args[0] == "-h" || args[0] == "--help" || args[0] == "help" {
        print!("{USAGE}");
        return;
    }
    if args[0] == "--version" || args[0] == "-V" {
        if cfg!(feature = "llvm") {
            println!("xeroc {} (LLVM built in)", env!("CARGO_PKG_VERSION"));
        } else {
            println!("xeroc {}", env!("CARGO_PKG_VERSION"));
        }
        return;
    }
    let cmd = args[0].as_str();
    if cmd == "targets" {
        for t in xeroc::target::TARGETS {
            println!("{:<16} {}", t.name, t.triple);
        }
        return;
    }
    if cmd == "fmt" {
        exit(run_fmt(&args[1..]));
    }
    if cmd == "new" {
        let lib = args.iter().any(|a| a == "--lib");
        let Some(name) = args[1..].iter().find(|a| !a.starts_with('-')) else {
            eprintln!("error: `xeroc new` needs a name: `xeroc new hello`");
            exit(2);
        };
        match xeroc::project::new_project(&PathBuf::from(name), lib) {
            Ok(n) => {
                println!(
                    "created {} `{n}` in {name}/",
                    if lib { "library" } else { "program" }
                );
                println!(
                    "    cd {name} && xeroc {}",
                    if lib { "test" } else { "run" }
                );
            }
            Err(e) => {
                eprintln!("error: {e}");
                exit(1);
            }
        }
        return;
    }
    if !matches!(cmd, "build" | "run" | "check" | "test") {
        eprintln!("error: unknown command `{cmd}`\n\n{USAGE}");
        exit(2);
    }
    let mut input = None;
    let mut output = None;
    let mut emit = Emit::Exe;
    let mut opt_level = 2;
    let mut overflow_checks = None;
    let mut debug = false;
    let mut target = xeroc::target::host();
    let mut lib = false;
    let mut intel = true;
    let mut libc = Libc::Auto;
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
            "-g" => debug = true,
            "--overflow-checks" => overflow_checks = Some(true),
            "--no-overflow-checks" => overflow_checks = Some(false),
            "--libc" => libc = Libc::Yes,
            "--no-libc" => libc = Libc::No,
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
    // no file: the project in this directory (or a parent)
    let mut project = None;
    let mut module_dirs = Vec::new();
    let mut packages = Vec::new();
    let mut test_filter = None;
    if cmd == "test" {
        test_filter = input.take().map(|p: PathBuf| p.display().to_string());
    }
    let input = match input {
        Some(i) => i,
        None => {
            let cwd = std::env::current_dir().unwrap_or_default();
            let p = xeroc::project::Project::find(&cwd).unwrap_or_else(|e| {
                eprintln!("error: {e}");
                exit(2);
            });
            packages = p.packages().unwrap_or_else(|e| {
                eprintln!("error: {e}");
                exit(1);
            });
            let (root, is_lib) = p.root().unwrap_or_else(|e| {
                eprintln!("error: {e}");
                exit(1);
            });
            module_dirs.push(p.src());
            // a library's tests import it by its name
            if is_lib {
                packages.push((p.manifest.name.clone(), p.src()));
            }
            if is_lib && matches!(cmd, "build" | "run") {
                if cmd == "run" {
                    eprintln!("error: `{}` is a library (src/lib.xr): there is nothing to run; try `xeroc test`", p.manifest.name);
                    exit(1);
                }
                lib = true;
            }
            if output.is_none() && cmd == "build" {
                let b = p.build_dir();
                let _ = std::fs::create_dir_all(&b);
                output = Some(if is_lib {
                    b.join(&p.manifest.name).with_extension("o")
                } else {
                    b.join(&p.manifest.name)
                        .with_extension(std::env::consts::EXE_EXTENSION)
                });
                if is_lib {
                    emit = Emit::Obj;
                }
            }
            project = Some(p);
            root
        }
    };
    if cmd == "test" {
        let Some(p) = project else {
            eprintln!("error: `xeroc test` runs a project's tests; run it in a directory with a xero.toml");
            exit(2);
        };
        let base = Options {
            input: input.clone(),
            output: None,
            emit: Emit::Exe,
            opt_level,
            target,
            lib: false,
            intel_syntax: intel,
            libc,
            overflow_checks: Some(overflow_checks.unwrap_or(true)),
            module_dirs,
            packages,
            root_suffix: None,
            debug,
        };
        exit(run_tests(&p, &base, test_filter.as_deref()));
    }
    if cmd == "check" {
        let opts = Options {
            input,
            output: None,
            emit: Emit::Xir,
            opt_level,
            target,
            lib,
            intel_syntax: intel,
            libc,
            overflow_checks,
            module_dirs,
            packages,
            root_suffix: None,
            debug,
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
        libc,
        overflow_checks,
        module_dirs,
        packages,
        root_suffix: None,
        debug,
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

/// `xeroc fmt`: format the named files, or a project's `src/` and
/// `tests/`. With `--check`, only report files that are not formatted.
fn run_fmt(args: &[String]) -> i32 {
    let check = args.iter().any(|a| a == "--check");
    let mut files: Vec<PathBuf> = args
        .iter()
        .filter(|a| !a.starts_with('-'))
        .map(PathBuf::from)
        .collect();
    if files.is_empty() {
        let cwd = std::env::current_dir().unwrap_or_default();
        let p = match xeroc::project::Project::find(&cwd) {
            Ok(p) => p,
            Err(e) => {
                eprintln!("error: {e}");
                return 2;
            }
        };
        for dir in [p.src(), p.dir.join("tests")] {
            collect_xr(&dir, &mut files);
        }
        files.sort();
    }
    let mut unformatted = 0;
    let mut failed = 0;
    for f in &files {
        let src = match std::fs::read_to_string(f) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("error: cannot read `{}`: {e}", f.display());
                failed += 1;
                continue;
            }
        };
        match xeroc::fmt::format_source(&src) {
            Ok(out) if out == src => {}
            Ok(out) => {
                if check {
                    println!("{}", f.display());
                    unformatted += 1;
                } else if let Err(e) = std::fs::write(f, out) {
                    eprintln!("error: cannot write `{}`: {e}", f.display());
                    failed += 1;
                } else {
                    println!("formatted {}", f.display());
                }
            }
            Err(e) => {
                eprintln!("error: {}: {e}", f.display());
                failed += 1;
            }
        }
    }
    if failed > 0 {
        1
    } else if check && unformatted > 0 {
        eprintln!("{unformatted} file(s) need formatting: run `xeroc fmt`");
        1
    } else {
        0
    }
}

fn collect_xr(dir: &std::path::Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            collect_xr(&p, out);
        } else if p.extension().is_some_and(|x| x == "xr") {
            out.push(p);
        }
    }
}

/// Build each `tests/*.xr` with a generated `main`, then run each of its
/// `fn test_...()` in its own process. Returns the exit code.
fn run_tests(p: &xeroc::project::Project, base: &Options, filter: Option<&str>) -> i32 {
    let dir = p.dir.join("tests");
    let mut files: Vec<PathBuf> = std::fs::read_dir(&dir)
        .map(|rd| {
            rd.filter_map(|e| e.ok().map(|e| e.path()))
                .filter(|f| f.extension().is_some_and(|e| e == "xr"))
                .collect()
        })
        .unwrap_or_default();
    files.sort();
    let out_dir = p.build_dir().join("tests");
    let _ = std::fs::create_dir_all(&out_dir);
    let (mut passed, mut failed, mut broken) = (0, Vec::new(), 0);
    for f in &files {
        let stem = f.file_stem().unwrap().to_string_lossy().to_string();
        let src = std::fs::read_to_string(f).unwrap_or_default();
        let names: Vec<String> = xeroc::project::test_names(&src)
            .into_iter()
            .filter(|n| filter.is_none_or(|w| n.contains(w) || stem.contains(w)))
            .collect();
        if names.is_empty() {
            continue;
        }
        let mut opts = base.clone();
        opts.input = f.clone();
        opts.root_suffix = Some(xeroc::project::test_main(&names));
        let exe = out_dir
            .join(&stem)
            .with_extension(std::env::consts::EXE_EXTENSION);
        opts.output = Some(exe.clone());
        if let Err(msg) = build(&opts) {
            eprint!("{msg}");
            eprintln!("error: tests/{stem}.xr does not compile");
            broken += 1;
            continue;
        }
        for n in &names {
            let r = Command::new(&exe).arg(n).output();
            match r {
                Ok(o) if o.status.success() => {
                    println!("test {stem}::{n} ... ok");
                    passed += 1;
                }
                Ok(o) => {
                    println!("test {stem}::{n} ... FAILED");
                    let out = String::from_utf8_lossy(&o.stdout);
                    let err = String::from_utf8_lossy(&o.stderr);
                    failed.push((format!("{stem}::{n}"), format!("{out}{err}")));
                }
                Err(e) => {
                    println!("test {stem}::{n} ... FAILED");
                    failed.push((format!("{stem}::{n}"), format!("cannot run: {e}\n")));
                }
            }
        }
    }
    for (n, out) in &failed {
        println!("\n---- {n} ----\n{}", out.trim_end());
    }
    let total = passed + failed.len();
    if total == 0 && broken == 0 {
        println!("no tests found (write `fn test_something()` in tests/*.xr)");
        return 0;
    }
    println!(
        "\n{} passed, {} failed{}",
        passed,
        failed.len(),
        if broken > 0 {
            format!(", {broken} test file(s) did not compile")
        } else {
            String::new()
        }
    );
    if failed.is_empty() && broken == 0 {
        0
    } else {
        1
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
