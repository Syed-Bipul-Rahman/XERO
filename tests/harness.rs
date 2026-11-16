//! Runs every program in `tests/run` and compares stdout (and the exit
//! code) against the `// output:` block at the end of the file, and checks
//! that every program in `tests/fail` is rejected with the diagnostics listed
//! in its `// error:` lines.

use std::path::{Path, PathBuf};
use std::process::Command;

fn xeroc() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_xeroc"))
}

fn files(dir: &str) -> Vec<PathBuf> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join(dir);
    let mut v: Vec<PathBuf> = std::fs::read_dir(&root)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|e| e == "xr"))
        .collect();
    v.sort();
    v
}

fn expected_output(src: &str) -> (String, i32) {
    let mut out = String::new();
    let mut code = 0;
    let mut in_block = false;
    for line in src.lines() {
        if line.trim() == "// output:" {
            in_block = true;
            continue;
        }
        if let Some(c) = line.trim().strip_prefix("// exit:") {
            code = c.trim().parse().unwrap();
            continue;
        }
        if in_block {
            let l = line
                .strip_prefix("// ")
                .or_else(|| line.strip_prefix("//"))
                .unwrap_or(line);
            out.push_str(l);
            out.push('\n');
        }
    }
    (out, code)
}

/// Extra `xeroc build` flags from `XERO_TEST_FLAGS` (e.g. `--libc`, or
/// `--target aarch64-linux` together with `XERO_TEST_RUNNER=qemu-aarch64`).
fn extra_flags() -> Vec<String> {
    std::env::var("XERO_TEST_FLAGS")
        .unwrap_or_default()
        .split_whitespace()
        .map(String::from)
        .collect()
}

#[test]
fn run_programs() {
    let tmp = std::env::temp_dir().join(format!("xero-tests-{}", std::process::id()));
    std::fs::create_dir_all(&tmp).unwrap();
    let mut failures = Vec::new();
    for f in files("tests/run") {
        let src = std::fs::read_to_string(&f).unwrap();
        let (want, want_code) = expected_output(&src);
        // programs that call C functions need the target's C library to link
        if src.contains("// test: native-only") && extra_flags().iter().any(|f| f == "--target") {
            continue;
        }
        let exe = tmp
            .join(f.file_stem().unwrap())
            .with_extension(std::env::consts::EXE_EXTENSION);
        // `// flags: --overflow-checks` in a test adds build flags
        let own_flags: Vec<String> = src
            .lines()
            .filter_map(|l| l.trim().strip_prefix("// flags:"))
            .flat_map(|f| f.split_whitespace().map(String::from).collect::<Vec<_>>())
            .collect();
        let build = Command::new(xeroc())
            .arg("build")
            .args(extra_flags())
            .args(&own_flags)
            .arg(&f)
            .arg("-o")
            .arg(&exe)
            .output()
            .unwrap();
        if !build.status.success() {
            failures.push(format!(
                "{}: compile failed:\n{}",
                f.display(),
                String::from_utf8_lossy(&build.stderr)
            ));
            continue;
        }
        let mut cmd = match std::env::var("XERO_TEST_RUNNER") {
            Ok(r) if !r.is_empty() => {
                let mut c = Command::new(r);
                c.arg(&exe);
                c
            }
            _ => Command::new(&exe),
        };
        let Some(run) = run_with_timeout(&mut cmd, 120) else {
            failures.push(format!("{}: timed out after 120 s", f.display()));
            continue;
        };
        // the Windows C runtime writes text-mode stdout with CRLF line endings
        let got = String::from_utf8_lossy(&run.stdout).replace("\r\n", "\n");
        let code = run.status.code().unwrap_or(-1);
        if got != want || code != want_code {
            failures.push(format!("{}: output mismatch (exit {code}, want {want_code})\n--- want\n{want}--- got\n{got}", f.display()));
        }
    }
    let _ = std::fs::remove_dir_all(&tmp);
    annotate(&failures);
    if !failures.is_empty() {
        panic!(
            "{} program(s) failed:\n\n{}",
            failures.len(),
            failures.join("\n")
        );
    }
}

/// Run `cmd`, killing it after `secs` seconds (then `None`).
fn run_with_timeout(cmd: &mut Command, secs: u64) -> Option<std::process::Output> {
    use std::process::Stdio;
    let mut child = cmd
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut out = child.stdout.take().unwrap();
    let mut err = child.stderr.take().unwrap();
    let o = std::thread::spawn(move || {
        let mut v = Vec::new();
        std::io::Read::read_to_end(&mut out, &mut v).ok();
        v
    });
    let e = std::thread::spawn(move || {
        let mut v = Vec::new();
        std::io::Read::read_to_end(&mut err, &mut v).ok();
        v
    });
    let start = std::time::Instant::now();
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            return Some(std::process::Output {
                status,
                stdout: o.join().unwrap(),
                stderr: e.join().unwrap(),
            });
        }
        if start.elapsed().as_secs() >= secs {
            let _ = child.kill();
            let _ = child.wait();
            return None;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
}

/// On GitHub Actions, report each failure as an error annotation too.
fn annotate(failures: &[String]) {
    if std::env::var_os("GITHUB_ACTIONS").is_none() {
        return;
    }
    for f in failures {
        let msg: String = f.chars().take(4000).collect();
        println!(
            "::error title=xero test::{}",
            msg.replace('%', "%25")
                .replace('\r', "")
                .replace('\n', "%0A")
        );
    }
}

#[test]
fn rejected_programs() {
    let mut failures = Vec::new();
    for f in files("tests/fail") {
        let src = std::fs::read_to_string(&f).unwrap();
        let wanted: Vec<&str> = src
            .lines()
            .filter_map(|l| l.trim().strip_prefix("// error:"))
            .map(|s| s.trim())
            .collect();
        assert!(
            !wanted.is_empty(),
            "{} has no `// error:` lines",
            f.display()
        );
        let out = Command::new(xeroc()).arg("check").arg(&f).output().unwrap();
        let stderr = String::from_utf8_lossy(&out.stderr);
        if out.status.success() {
            failures.push(format!(
                "{}: compiled successfully but should have been rejected",
                f.display()
            ));
            continue;
        }
        for w in wanted {
            if !stderr.contains(w) {
                failures.push(format!(
                    "{}: expected error containing `{w}`, got:\n{stderr}",
                    f.display()
                ));
            }
        }
    }
    annotate(&failures);
    if !failures.is_empty() {
        panic!(
            "{} program(s) failed:\n\n{}",
            failures.len(),
            failures.join("\n")
        );
    }
}

/// `xeroc new`, then `build`, `run` and `test` in the project, with a
/// library it depends on by path.
#[test]
fn project_workflow() {
    let tmp = std::env::temp_dir().join(format!("xero-proj-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).unwrap();
    let x = |dir: &Path, args: &[&str]| {
        Command::new(xeroc())
            .args(args)
            .current_dir(dir)
            .output()
            .unwrap()
    };
    let ok = |o: &std::process::Output, what: &str| {
        assert!(
            o.status.success(),
            "{what} failed:\n{}{}",
            String::from_utf8_lossy(&o.stdout),
            String::from_utf8_lossy(&o.stderr)
        );
        String::from_utf8_lossy(&o.stdout).replace("\r\n", "\n")
    };
    ok(&x(&tmp, &["new", "app"]), "new app");
    ok(&x(&tmp, &["new", "--lib", "mathlib"]), "new --lib");
    std::fs::write(
        tmp.join("mathlib/src/lib.xr"),
        "module mathlib\n\npub fn triple(x: i32) -> i32 {\n    x * 3\n}\n",
    )
    .unwrap();
    std::fs::write(
        tmp.join("mathlib/tests/basics.xr"),
        "import mathlib\n\nfn test_triple() {\n    assert_eq(mathlib.triple(2), 6)\n}\n\nfn test_broken() {\n    assert_eq(mathlib.triple(2), 7)\n}\n",
    )
    .unwrap();
    let app = tmp.join("app");
    let mut manifest = std::fs::read_to_string(app.join("xero.toml")).unwrap();
    manifest.push_str("mathlib = { path = \"../mathlib\" }\n");
    std::fs::write(app.join("xero.toml"), manifest).unwrap();
    std::fs::write(
        app.join("src/main.xr"),
        "import greeting\nimport mathlib\n\nfn main() {\n    println(greeting.hello(\"Xero\"))\n    println(mathlib.triple(14))\n}\n",
    )
    .unwrap();
    let out = ok(&x(&app, &["run"]), "run");
    assert_eq!(out, "Hello, Xero!\n42\n");
    ok(&x(&app, &["build"]), "build");
    let exe = app
        .join("build/app")
        .with_extension(std::env::consts::EXE_EXTENSION);
    let built = Command::new(&exe).output().unwrap();
    assert_eq!(
        String::from_utf8_lossy(&built.stdout).replace("\r\n", "\n"),
        "Hello, Xero!\n42\n"
    );
    let t = ok(&x(&app, &["test"]), "test");
    assert!(t.contains("test greeting::test_hello ... ok"), "{t}");
    // one test passes, one fails: exit code 1 and the assertion shown
    let lib = x(&tmp.join("mathlib"), &["test"]);
    assert!(!lib.status.success());
    let lt = String::from_utf8_lossy(&lib.stdout);
    assert!(lt.contains("test basics::test_triple ... ok"), "{lt}");
    assert!(lt.contains("test basics::test_broken ... FAILED"), "{lt}");
    assert!(lt.contains("left: 6"), "{lt}");
    assert!(lt.contains("1 passed, 1 failed"), "{lt}");
    let only = ok(&x(&tmp.join("mathlib"), &["test", "triple"]), "test filter");
    assert!(only.contains("1 passed, 0 failed"), "{only}");
    let _ = std::fs::remove_dir_all(&tmp);
}

/// With overflow checks, `+` that overflows panics with a message; without
/// them it wraps.
#[test]
fn overflow_checks() {
    let tmp = std::env::temp_dir().join(format!("xero-ovf-{}", std::process::id()));
    std::fs::create_dir_all(&tmp).unwrap();
    let src = tmp.join("ovf.xr");
    std::fs::write(
        &src,
        "fn big() -> i32 { 2147483600 }\nfn main() {\n    println(big() + 48)\n}\n",
    )
    .unwrap();
    for (flag, wraps) in [
        ("--overflow-checks", false),
        ("--no-overflow-checks", true),
        ("-O0", false),
    ] {
        let exe = tmp
            .join("ovf")
            .with_extension(std::env::consts::EXE_EXTENSION);
        let build = Command::new(xeroc())
            .args(["build", flag])
            .arg(&src)
            .arg("-o")
            .arg(&exe)
            .output()
            .unwrap();
        assert!(
            build.status.success(),
            "{}",
            String::from_utf8_lossy(&build.stderr)
        );
        let run = Command::new(&exe).output().unwrap();
        let out = String::from_utf8_lossy(&run.stdout);
        let err = String::from_utf8_lossy(&run.stderr);
        if wraps {
            assert!(
                run.status.success() && out.trim() == "-2147483648",
                "{flag}: {out} {err}"
            );
        } else {
            assert!(!run.status.success(), "{flag}: should have panicked");
            assert!(
                err.contains("panic: attempt to add with overflow"),
                "{flag}: {err}"
            );
        }
    }
    let _ = std::fs::remove_dir_all(&tmp);
}

/// `fn add(a: i32, b: i32) -> i32 { a + b }` must compile to straight-line
/// code with no calls, allocation or runtime support.
#[test]
fn add_is_zero_cost() {
    let tmp = std::env::temp_dir().join(format!("xero-asm-{}", std::process::id()));
    std::fs::create_dir_all(&tmp).unwrap();
    let src = tmp.join("add.xr");
    std::fs::write(
        &src,
        "export fn add(a: i32, b: i32) -> i32 {\n    a + b\n}\n",
    )
    .unwrap();
    let asm = tmp.join("add.s");
    let out = Command::new(xeroc())
        .args([
            "build",
            "--lib",
            "--emit",
            "asm",
            "--target",
            "x86_64-linux",
            "-o",
        ])
        .arg(&asm)
        .arg(&src)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let text = std::fs::read_to_string(&asm).unwrap();
    let body: Vec<&str> = text
        .lines()
        .skip_while(|l| !l.starts_with("add:"))
        .skip(1)
        .take_while(|l| !l.starts_with(".Lfunc_end"))
        .map(|l| l.trim())
        .filter(|l| !l.is_empty() && !l.starts_with('#') && !l.starts_with('.'))
        .collect();
    assert_eq!(
        body,
        vec!["lea\teax, [rdi + rsi]", "ret"],
        "unexpected code for add:\n{text}"
    );
    let _ = std::fs::remove_dir_all(&tmp);
}

/// On Linux, programs that declare no C functions run on Xero's own runtime:
/// a small static executable with no dynamic loader and no libc.
#[test]
#[cfg(target_os = "linux")]
fn linux_programs_need_no_libc() {
    let tmp = std::env::temp_dir().join(format!("xero-static-{}", std::process::id()));
    std::fs::create_dir_all(&tmp).unwrap();
    let exe = tmp.join("hello");
    let hello = Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/hello.xr");
    let out = Command::new(xeroc())
        .arg("build")
        .arg(&hello)
        .arg("-o")
        .arg(&exe)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let bytes = std::fs::read(&exe).unwrap();
    // ELF program headers: no PT_INTERP (3) and no PT_DYNAMIC (2)
    let phoff = u64::from_le_bytes(bytes[32..40].try_into().unwrap()) as usize;
    let phentsize = u16::from_le_bytes(bytes[54..56].try_into().unwrap()) as usize;
    let phnum = u16::from_le_bytes(bytes[56..58].try_into().unwrap()) as usize;
    for i in 0..phnum {
        let at = phoff + i * phentsize;
        let kind = u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap());
        assert!(kind != 2 && kind != 3, "hello is dynamically linked");
    }
    assert!(bytes.len() < 32 * 1024, "hello is {} bytes", bytes.len());
    let run = Command::new(&exe).output().unwrap();
    assert_eq!(String::from_utf8_lossy(&run.stdout), "Hello, Xero!\n");
    let _ = std::fs::remove_dir_all(&tmp);
}
