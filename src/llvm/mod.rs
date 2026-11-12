//! LLVM and lld inside xeroc (the `llvm` feature): turning the generated
//! LLVM IR into machine code, and linking, without running clang.

use std::ffi::{c_char, c_int, c_void, CStr, CString};
use std::path::Path;
use std::sync::Once;

type Ctx = *mut c_void;
type Module = *mut c_void;
type MemBuf = *mut c_void;
type Target = *mut c_void;
type TargetMachine = *mut c_void;
type TargetData = *mut c_void;
type PassOptions = *mut c_void;
type Error = *mut c_void;

extern "C" {
    fn LLVMContextCreate() -> Ctx;
    fn LLVMContextDispose(c: Ctx);
    fn LLVMCreateMemoryBufferWithMemoryRangeCopy(
        data: *const c_char,
        len: usize,
        name: *const c_char,
    ) -> MemBuf;
    fn LLVMParseIRInContext(c: Ctx, buf: MemBuf, out: *mut Module, msg: *mut *mut c_char) -> c_int;
    fn LLVMDisposeModule(m: Module);
    fn LLVMVerifyModule(m: Module, action: c_int, msg: *mut *mut c_char) -> c_int;
    fn LLVMPrintModuleToString(m: Module) -> *mut c_char;
    fn LLVMDisposeMessage(m: *mut c_char);
    fn LLVMSetTarget(m: Module, triple: *const c_char);
    fn LLVMSetDataLayout(m: Module, dl: *const c_char);
    fn LLVMGetTargetFromTriple(
        triple: *const c_char,
        t: *mut Target,
        msg: *mut *mut c_char,
    ) -> c_int;
    fn LLVMCreateTargetMachine(
        t: Target,
        triple: *const c_char,
        cpu: *const c_char,
        features: *const c_char,
        level: c_int,
        reloc: c_int,
        model: c_int,
    ) -> TargetMachine;
    fn LLVMDisposeTargetMachine(tm: TargetMachine);
    fn LLVMCreateTargetDataLayout(tm: TargetMachine) -> TargetData;
    fn LLVMCopyStringRepOfTargetData(td: TargetData) -> *mut c_char;
    fn LLVMDisposeTargetData(td: TargetData);
    fn LLVMCreatePassBuilderOptions() -> PassOptions;
    fn LLVMDisposePassBuilderOptions(o: PassOptions);
    fn LLVMRunPasses(m: Module, passes: *const c_char, tm: TargetMachine, o: PassOptions) -> Error;
    fn LLVMGetErrorMessage(e: Error) -> *mut c_char;
    fn LLVMDisposeErrorMessage(m: *mut c_char);
    fn LLVMTargetMachineEmitToMemoryBuffer(
        tm: TargetMachine,
        m: Module,
        kind: c_int,
        msg: *mut *mut c_char,
        out: *mut MemBuf,
    ) -> c_int;
    fn LLVMGetBufferStart(b: MemBuf) -> *const c_char;
    fn LLVMGetBufferSize(b: MemBuf) -> usize;
    fn LLVMDisposeMemoryBuffer(b: MemBuf);
    fn LLVMParseCommandLineOptions(
        argc: c_int,
        argv: *const *const c_char,
        overview: *const c_char,
    );

    fn LLVMInitializeX86TargetInfo();
    fn LLVMInitializeX86Target();
    fn LLVMInitializeX86TargetMC();
    fn LLVMInitializeX86AsmPrinter();
    fn LLVMInitializeX86AsmParser();
    fn LLVMInitializeAArch64TargetInfo();
    fn LLVMInitializeAArch64Target();
    fn LLVMInitializeAArch64TargetMC();
    fn LLVMInitializeAArch64AsmPrinter();
    fn LLVMInitializeAArch64AsmParser();
    fn LLVMInitializeRISCVTargetInfo();
    fn LLVMInitializeRISCVTarget();
    fn LLVMInitializeRISCVTargetMC();
    fn LLVMInitializeRISCVAsmPrinter();
    fn LLVMInitializeRISCVAsmParser();
    fn LLVMInitializeWebAssemblyTargetInfo();
    fn LLVMInitializeWebAssemblyTarget();
    fn LLVMInitializeWebAssemblyTargetMC();
    fn LLVMInitializeWebAssemblyAsmPrinter();
    fn LLVMInitializeWebAssemblyAsmParser();
    fn LLVMInitializeARMTargetInfo();
    fn LLVMInitializeARMTarget();
    fn LLVMInitializeARMTargetMC();
    fn LLVMInitializeARMAsmPrinter();
    fn LLVMInitializeARMAsmParser();

    fn xero_lld_main(argc: c_int, argv: *const *const c_char) -> c_int;
}

/// What to produce from LLVM IR.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Output {
    Object,
    Assembly,
    /// Optimized LLVM IR (text).
    Ir,
}

static INIT: Once = Once::new();

fn init(intel_syntax: bool) {
    INIT.call_once(|| unsafe {
        LLVMInitializeX86TargetInfo();
        LLVMInitializeX86Target();
        LLVMInitializeX86TargetMC();
        LLVMInitializeX86AsmPrinter();
        LLVMInitializeX86AsmParser();
        LLVMInitializeAArch64TargetInfo();
        LLVMInitializeAArch64Target();
        LLVMInitializeAArch64TargetMC();
        LLVMInitializeAArch64AsmPrinter();
        LLVMInitializeAArch64AsmParser();
        LLVMInitializeRISCVTargetInfo();
        LLVMInitializeRISCVTarget();
        LLVMInitializeRISCVTargetMC();
        LLVMInitializeRISCVAsmPrinter();
        LLVMInitializeRISCVAsmParser();
        LLVMInitializeWebAssemblyTargetInfo();
        LLVMInitializeWebAssemblyTarget();
        LLVMInitializeWebAssemblyTargetMC();
        LLVMInitializeWebAssemblyAsmPrinter();
        LLVMInitializeWebAssemblyAsmParser();
        LLVMInitializeARMTargetInfo();
        LLVMInitializeARMTarget();
        LLVMInitializeARMTargetMC();
        LLVMInitializeARMAsmPrinter();
        LLVMInitializeARMAsmParser();
        if intel_syntax {
            let a0 = CString::new("xeroc").unwrap();
            let a1 = CString::new("-x86-asm-syntax=intel").unwrap();
            let argv = [a0.as_ptr(), a1.as_ptr()];
            LLVMParseCommandLineOptions(2, argv.as_ptr(), std::ptr::null());
        }
    });
}

unsafe fn take_msg(p: *mut c_char) -> String {
    if p.is_null() {
        return String::new();
    }
    let s = CStr::from_ptr(p).to_string_lossy().into_owned();
    LLVMDisposeMessage(p);
    s
}

/// The CPU and features clang would choose for `triple`.
fn cpu_features(triple: &str) -> (&'static str, &'static str) {
    let arch = triple.split('-').next().unwrap_or("");
    match arch {
        "x86_64" => ("x86-64", ""),
        "riscv64" => ("generic-rv64", "+m,+a,+f,+d,+c"),
        "riscv32" => ("generic-rv32", "+m,+a,+c"),
        "thumbv7em" => ("cortex-m4", ""),
        _ => ("generic", ""),
    }
}

/// Optimize LLVM IR `ll` and generate `out` for `triple`.
pub fn compile(
    ll: &str,
    triple: &str,
    opt: u8,
    pic: bool,
    out: Output,
    intel_syntax: bool,
) -> Result<Vec<u8>, String> {
    init(intel_syntax);
    unsafe {
        let ctx = LLVMContextCreate();
        let name = CString::new("xero").unwrap();
        let buf = LLVMCreateMemoryBufferWithMemoryRangeCopy(
            ll.as_ptr() as *const c_char,
            ll.len(),
            name.as_ptr(),
        );
        let mut m: Module = std::ptr::null_mut();
        let mut msg: *mut c_char = std::ptr::null_mut();
        if LLVMParseIRInContext(ctx, buf, &mut m, &mut msg) != 0 {
            let e = take_msg(msg);
            LLVMContextDispose(ctx);
            return Err(format!("internal error: invalid LLVM IR: {e}"));
        }
        let r = compile_module(m, triple, opt, pic, out);
        LLVMDisposeModule(m);
        LLVMContextDispose(ctx);
        r
    }
}

unsafe fn compile_module(
    m: Module,
    triple: &str,
    opt: u8,
    pic: bool,
    out: Output,
) -> Result<Vec<u8>, String> {
    let mut msg: *mut c_char = std::ptr::null_mut();
    if LLVMVerifyModule(m, 2, &mut msg) != 0 {
        return Err(format!(
            "internal error: xeroc generated invalid LLVM IR:\n{}",
            take_msg(msg)
        ));
    }
    take_msg(msg);
    let ctriple = CString::new(triple).unwrap();
    let mut target: Target = std::ptr::null_mut();
    if LLVMGetTargetFromTriple(ctriple.as_ptr(), &mut target, &mut msg) != 0 {
        return Err(format!("unsupported target `{triple}`: {}", take_msg(msg)));
    }
    let (cpu, features) = cpu_features(triple);
    let (ccpu, cfeat) = (CString::new(cpu).unwrap(), CString::new(features).unwrap());
    let level = match opt {
        0 => 0,
        1 => 1,
        2 => 2,
        _ => 3,
    };
    let tm = LLVMCreateTargetMachine(
        target,
        ctriple.as_ptr(),
        ccpu.as_ptr(),
        cfeat.as_ptr(),
        level,
        if pic { 2 } else { 1 },
        0,
    );
    if tm.is_null() {
        return Err(format!("cannot create a code generator for `{triple}`"));
    }
    LLVMSetTarget(m, ctriple.as_ptr());
    let td = LLVMCreateTargetDataLayout(tm);
    let dl = LLVMCopyStringRepOfTargetData(td);
    LLVMSetDataLayout(m, dl);
    LLVMDisposeMessage(dl);
    LLVMDisposeTargetData(td);
    // the optimization pipeline clang uses for -O<n>
    let pipeline = CString::new(match opt {
        0 => "default<O0>",
        1 => "default<O1>",
        2 => "default<O2>",
        b's' => "default<Os>",
        _ => "default<O3>",
    })
    .unwrap();
    let po = LLVMCreatePassBuilderOptions();
    let err = LLVMRunPasses(m, pipeline.as_ptr(), tm, po);
    LLVMDisposePassBuilderOptions(po);
    if !err.is_null() {
        let p = LLVMGetErrorMessage(err);
        let s = CStr::from_ptr(p).to_string_lossy().into_owned();
        LLVMDisposeErrorMessage(p);
        LLVMDisposeTargetMachine(tm);
        return Err(format!("LLVM optimization failed: {s}"));
    }
    let result = if out == Output::Ir {
        let p = LLVMPrintModuleToString(m);
        Ok(take_msg(p).into_bytes())
    } else {
        let mut buf: MemBuf = std::ptr::null_mut();
        let kind = if out == Output::Assembly { 0 } else { 1 };
        if LLVMTargetMachineEmitToMemoryBuffer(tm, m, kind, &mut msg, &mut buf) != 0 {
            Err(format!("LLVM code generation failed: {}", take_msg(msg)))
        } else {
            let bytes = std::slice::from_raw_parts(
                LLVMGetBufferStart(buf) as *const u8,
                LLVMGetBufferSize(buf),
            )
            .to_vec();
            LLVMDisposeMemoryBuffer(buf);
            Ok(bytes)
        }
    };
    LLVMDisposeTargetMachine(tm);
    result
}

/// Run lld in-process. `flavor` is "ld.lld", "lld-link", "ld64.lld" or "wasm-ld".
pub fn lld(flavor: &str, args: &[String]) -> Result<(), String> {
    let mut owned = vec![CString::new(flavor).unwrap()];
    for a in args {
        owned.push(
            CString::new(a.as_str()).map_err(|_| "linker argument contains NUL".to_string())?,
        );
    }
    let argv: Vec<*const c_char> = owned.iter().map(|s| s.as_ptr()).collect();
    let code = unsafe { xero_lld_main(argv.len() as c_int, argv.as_ptr()) };
    if code == 0 {
        Ok(())
    } else {
        Err(format!("linking failed ({flavor} exited with {code})"))
    }
}

/// Write `bytes` to `path`.
pub fn write(path: &Path, bytes: &[u8]) -> Result<(), String> {
    std::fs::write(path, bytes).map_err(|e| format!("cannot write `{}`: {e}", path.display()))
}
