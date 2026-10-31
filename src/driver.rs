//! Compiler driver: module loading, the front-end pipeline, and invoking
//! LLVM (through clang) to produce assembly, objects and executables.

use std::collections::{HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::check::{Checker, ParsedModule, Target};
use crate::diag::{Diagnostic, SourceMap, Span};
use crate::target::TargetInfo;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Emit {
    Exe,
    Obj,
    Asm,
    /// Optimized LLVM IR.
    Llvm,
    /// LLVM IR exactly as xeroc generates it, before optimization.
    LlvmRaw,
    /// Xero's own IR (debug dump).
    Xir,
}

#[derive(Clone, Debug)]
pub struct Options {
    pub input: PathBuf,
    pub output: Option<PathBuf>,
    pub emit: Emit,
    pub opt_level: u8,
    pub target: TargetInfo,
    pub lib: bool,
    pub intel_syntax: bool,
}

/// Standard library modules, embedded in the compiler binary.
pub const STD_MODULES: &[(&str, &str)] = &[
    ("std.prelude", include_str!("../std/prelude.xr")),
    ("std.io", include_str!("../std/io.xr")),
    ("std.mem", include_str!("../std/mem.xr")),
    ("std.vec", include_str!("../std/vec.xr")),
    ("std.string", include_str!("../std/string.xr")),
    ("std.math", include_str!("../std/math.xr")),
];

pub struct Frontend {
    pub sm: SourceMap,
}

pub enum FrontendError {
    Diags(Vec<Diagnostic>),
    Io(String),
}

impl Frontend {
    pub fn render(&self, ds: &[Diagnostic]) -> String {
        let mut out = String::new();
        for d in ds {
            out.push_str(&d.render(&self.sm));
            out.push('\n');
        }
        let n = ds.len();
        out.push_str(&format!(
            "error: could not compile due to {} previous error{}\n",
            n,
            if n == 1 { "" } else { "s" }
        ));
        out
    }
}

fn parse_file(
    sm: &mut SourceMap,
    path: String,
    text: String,
) -> Result<crate::ast::Module, Diagnostic> {
    let file = sm.add(path, text);
    let toks = crate::lexer::lex(file, &sm.file(file).text)?;
    crate::parser::parse_module(file, toks)
}

/// Load the root file and everything it (transitively) imports.
pub fn load(fe: &mut Frontend, root: &Path) -> Result<(Vec<ParsedModule>, usize), FrontendError> {
    let text = std::fs::read_to_string(root)
        .map_err(|e| FrontendError::Io(format!("cannot read `{}`: {e}", root.display())))?;
    let root_dir = root.parent().map(|p| p.to_path_buf()).unwrap_or_default();
    let ast = parse_file(&mut fe.sm, root.display().to_string(), text)
        .map_err(|d| FrontendError::Diags(vec![d]))?;
    let root_name = ast
        .name
        .as_ref()
        .map(|n| n.name.clone())
        .unwrap_or_else(|| "main".into());
    let mut modules = vec![ParsedModule {
        path: root_name.clone(),
        ast,
        is_std: false,
    }];
    let mut seen: HashSet<String> = HashSet::new();
    seen.insert(root_name);
    let mut queue: VecDeque<usize> = VecDeque::from([0]);
    let mut want = |name: &str,
                    span: Span,
                    modules: &mut Vec<ParsedModule>,
                    queue: &mut VecDeque<usize>,
                    sm: &mut SourceMap|
     -> Result<(), Diagnostic> {
        if !seen.insert(name.to_string()) {
            return Ok(());
        }
        let (path, text, is_std) =
            if let Some((_, src)) = STD_MODULES.iter().find(|(n, _)| *n == name) {
                (format!("<{name}>"), src.to_string(), true)
            } else if name.starts_with("std.") {
                return Err(Diagnostic::new(
                    span,
                    format!("unknown standard library module `{name}`"),
                ));
            } else {
                let mut p = root_dir.clone();
                for seg in name.split('.') {
                    p.push(seg);
                }
                p.set_extension("xr");
                let text = std::fs::read_to_string(&p).map_err(|_| {
                    Diagnostic::new(
                        span,
                        format!("module `{name}` not found (looked for `{}`)", p.display()),
                    )
                })?;
                (p.display().to_string(), text, false)
            };
        let ast = parse_file(sm, path, text)?;
        modules.push(ParsedModule {
            path: name.to_string(),
            ast,
            is_std,
        });
        queue.push_back(modules.len() - 1);
        Ok(())
    };
    for m in ["std.prelude", "std.mem", "std.vec", "std.string"] {
        want(m, Span::default(), &mut modules, &mut queue, &mut fe.sm)
            .map_err(|d| FrontendError::Diags(vec![d]))?;
    }
    while let Some(i) = queue.pop_front() {
        let imports: Vec<(String, Span)> = modules[i]
            .ast
            .items
            .iter()
            .filter_map(|it| match &it.kind {
                crate::ast::ItemKind::Import(imp) => Some((
                    imp.path
                        .iter()
                        .map(|s| s.name.as_str())
                        .collect::<Vec<_>>()
                        .join("."),
                    it.span,
                )),
                _ => None,
            })
            .collect();
        for (name, span) in imports {
            want(&name, span, &mut modules, &mut queue, &mut fe.sm)
                .map_err(|d| FrontendError::Diags(vec![d]))?;
        }
    }
    Ok((modules, 0))
}

/// Run the front end and produce LLVM IR.
pub fn compile_to_llvm(
    fe: &mut Frontend,
    opts: &Options,
) -> Result<(String, String), FrontendError> {
    let (modules, root) = load(fe, &opts.input)?;
    let ck = Checker::new(Target {
        ptr_bits: opts.target.ptr_bits,
    });
    let (prog, ck) = ck
        .run(modules, root, opts.lib)
        .map_err(FrontendError::Diags)?;
    let xir_dump = if opts.emit == Emit::Xir {
        format!("{:#?}", prog)
    } else {
        String::new()
    };
    let ll = crate::codegen::generate(&ck, &prog, opts.target.triple, opts.target.ptr_bits);
    Ok((ll, xir_dump))
}

/// Oldest LLVM whose IR syntax (opaque `ptr` types) xeroc's output requires.
pub const MIN_LLVM: u32 = 15;

/// Major version from `clang --version` output ("... clang version 18.1.3 ...").
fn clang_major(cmd: &str) -> Option<u32> {
    let out = Command::new(cmd).arg("--version").output().ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    let rest = &text[text.find("clang version ")? + "clang version ".len()..];
    rest.split(['.', ' ']).next()?.parse().ok()
}

/// Find a clang new enough for xeroc. `XERO_CLANG` wins; otherwise prefer
/// plain `clang`, then versioned binaries (`clang-21` ... `clang-15`).
pub fn find_clang() -> Result<String, String> {
    if let Ok(c) = std::env::var("XERO_CLANG") {
        if !c.is_empty() {
            return Ok(c);
        }
    }
    let mut too_old = None;
    let candidates = std::iter::once("clang".to_string()).chain((MIN_LLVM..=24).rev().map(|v| format!("clang-{v}")));
    for c in candidates {
        match clang_major(&c) {
            Some(v) if v >= MIN_LLVM => return Ok(c),
            // Apple clang reports its own version scheme; any recent Xcode works
            Some(v) if cfg!(target_os = "macos") && c == "clang" && v >= 14 => return Ok(c),
            Some(v) => too_old = too_old.or(Some((c, v))),
            None => {}
        }
    }
    Err(match too_old {
        Some((c, v)) => format!(
            "error: `{c}` is LLVM {v}, but Xero needs LLVM {MIN_LLVM} or newer\n  \
             install a newer clang (e.g. `sudo apt install clang-18`) or set XERO_CLANG\n"
        ),
        None => "error: clang not found; Xero uses clang (LLVM 15+) as its code generator\n  \
                 see https://github.com/Syed-Bipul-Rahman/XERO/blob/main/INSTALL.md or set XERO_CLANG\n"
            .to_string(),
    })
}

/// Build according to `opts`. Returns the output path.
pub fn build(opts: &Options) -> Result<PathBuf, String> {
    let mut fe = Frontend {
        sm: SourceMap::default(),
    };
    let (ll, xir) = match compile_to_llvm(&mut fe, opts) {
        Ok(r) => r,
        Err(FrontendError::Diags(ds)) => return Err(fe.render(&ds)),
        Err(FrontendError::Io(e)) => return Err(format!("error: {e}\n")),
    };
    let stem = opts
        .input
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("out")
        .to_string();
    let default_out = match opts.emit {
        Emit::Exe => {
            if opts.target.triple.contains("windows") {
                format!("{stem}.exe")
            } else if opts.target.triple.starts_with("wasm") {
                format!("{stem}.wasm")
            } else {
                stem.clone()
            }
        }
        Emit::Obj => format!("{stem}.o"),
        Emit::Asm => format!("{stem}.s"),
        Emit::Llvm | Emit::LlvmRaw => format!("{stem}.ll"),
        Emit::Xir => format!("{stem}.xir"),
    };
    let out = opts
        .output
        .clone()
        .unwrap_or_else(|| PathBuf::from(default_out));
    match opts.emit {
        Emit::LlvmRaw => {
            std::fs::write(&out, ll)
                .map_err(|e| format!("error: cannot write `{}`: {e}\n", out.display()))?;
            return Ok(out);
        }
        Emit::Xir => {
            std::fs::write(&out, xir)
                .map_err(|e| format!("error: cannot write `{}`: {e}\n", out.display()))?;
            return Ok(out);
        }
        _ => {}
    }
    let tmpdir = std::env::temp_dir().join(format!("xeroc-{}-{}", std::process::id(), stem));
    std::fs::create_dir_all(&tmpdir).map_err(|e| format!("error: {e}\n"))?;
    let ll_path = tmpdir.join(format!("{stem}.ll"));
    std::fs::write(&ll_path, &ll).map_err(|e| format!("error: {e}\n"))?;
    let mut cmd = Command::new(find_clang()?);
    cmd.arg("-Wno-override-module")
        .arg(format!("--target={}", opts.target.triple))
        .arg(format!("-O{}", opts.opt_level))
        .arg("-x")
        .arg("ir")
        .arg(&ll_path)
        .arg("-o")
        .arg(&out);
    let is_x86 = opts.target.triple.starts_with("x86_64");
    match opts.emit {
        Emit::Asm => {
            cmd.arg("-S");
            if is_x86 && opts.intel_syntax {
                cmd.arg("-masm=intel");
            }
        }
        Emit::Obj => {
            cmd.arg("-c");
        }
        Emit::Llvm => {
            cmd.arg("-S").arg("-emit-llvm");
        }
        Emit::Exe => {
            if opts.target.triple.starts_with("wasm") {
                cmd.arg("-nostdlib")
                    .arg("-Wl,--no-entry")
                    .arg("-Wl,--export-dynamic");
            } else if !opts.target.hosted {
                return Err(format!("error: target `{}` has no operating system; use `--emit obj` and link with your own startup code\n", opts.target.name));
            }
            if opts.lib && !opts.target.triple.starts_with("wasm") {
                return Err("error: `--lib` produces an object; use `--emit obj`\n".into());
            }
        }
        _ => {}
    }
    if opts.target.triple.starts_with("wasm") || !opts.target.hosted {
        cmd.arg("-ffreestanding");
    }
    let output = cmd.output().map_err(|e| {
        format!("error: failed to run clang ({e}); install LLVM/clang or set XERO_CLANG\n")
    })?;
    let _ = std::fs::remove_dir_all(&tmpdir);
    if !output.status.success() {
        return Err(format!(
            "error: LLVM backend failed\n{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    Ok(out)
}
