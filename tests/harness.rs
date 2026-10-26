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
            let l = line.strip_prefix("// ").or_else(|| line.strip_prefix("//")).unwrap_or(line);
            out.push_str(l);
            out.push('\n');
        }
    }
    (out, code)
}

#[test]
fn run_programs() {
    let tmp = std::env::temp_dir().join(format!("xero-tests-{}", std::process::id()));
    std::fs::create_dir_all(&tmp).unwrap();
    let mut failures = Vec::new();
    for f in files("tests/run") {
        let src = std::fs::read_to_string(&f).unwrap();
        let (want, want_code) = expected_output(&src);
        let exe = tmp.join(f.file_stem().unwrap());
        let build = Command::new(xeroc()).arg("build").arg(&f).arg("-o").arg(&exe).output().unwrap();
        if !build.status.success() {
            failures.push(format!("{}: compile failed:\n{}", f.display(), String::from_utf8_lossy(&build.stderr)));
            continue;
        }
        let run = Command::new(&exe).output().unwrap();
        let got = String::from_utf8_lossy(&run.stdout).to_string();
        let code = run.status.code().unwrap_or(-1);
        if got != want || code != want_code {
            failures.push(format!("{}: output mismatch (exit {code}, want {want_code})\n--- want\n{want}--- got\n{got}", f.display()));
        }
    }
    let _ = std::fs::remove_dir_all(&tmp);
    if !failures.is_empty() {
        panic!("{} program(s) failed:\n\n{}", failures.len(), failures.join("\n"));
    }
}

#[test]
fn rejected_programs() {
    let mut failures = Vec::new();
    for f in files("tests/fail") {
        let src = std::fs::read_to_string(&f).unwrap();
        let wanted: Vec<&str> = src.lines().filter_map(|l| l.trim().strip_prefix("// error:")).map(|s| s.trim()).collect();
        assert!(!wanted.is_empty(), "{} has no `// error:` lines", f.display());
        let out = Command::new(xeroc()).arg("check").arg(&f).output().unwrap();
        let stderr = String::from_utf8_lossy(&out.stderr);
        if out.status.success() {
            failures.push(format!("{}: compiled successfully but should have been rejected", f.display()));
            continue;
        }
        for w in wanted {
            if !stderr.contains(w) {
                failures.push(format!("{}: expected error containing `{w}`, got:\n{stderr}", f.display()));
            }
        }
    }
    if !failures.is_empty() {
        panic!("{} program(s) failed:\n\n{}", failures.len(), failures.join("\n"));
    }
}

/// `fn add(a: i32, b: i32) -> i32 { a + b }` must compile to straight-line
/// code with no calls, allocation or runtime support.
#[test]
fn add_is_zero_cost() {
    let tmp = std::env::temp_dir().join(format!("xero-asm-{}", std::process::id()));
    std::fs::create_dir_all(&tmp).unwrap();
    let src = tmp.join("add.xr");
    std::fs::write(&src, "export fn add(a: i32, b: i32) -> i32 {\n    a + b\n}\n").unwrap();
    let asm = tmp.join("add.s");
    let out = Command::new(xeroc()).args(["build", "--lib", "--emit", "asm", "--target", "x86_64-linux", "-o"]).arg(&asm).arg(&src).output().unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let text = std::fs::read_to_string(&asm).unwrap();
    let body: Vec<&str> = text
        .lines()
        .skip_while(|l| !l.starts_with("add:"))
        .skip(1)
        .take_while(|l| !l.starts_with(".Lfunc_end"))
        .map(|l| l.trim())
        .filter(|l| !l.is_empty() && !l.starts_with('#') && !l.starts_with('.'))
        .collect();
    assert_eq!(body, vec!["lea\teax, [rdi + rsi]", "ret"], "unexpected code for add:\n{text}");
    let _ = std::fs::remove_dir_all(&tmp);
}
