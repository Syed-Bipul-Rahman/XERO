// Calls lld as a library, so xeroc links without an external linker.
// argv[0] selects the flavor: "ld.lld" (ELF), "lld-link" (COFF),
// "ld64.lld" (Mach-O) or "wasm-ld".

#include "lld/Common/Driver.h"
#include "llvm/Support/raw_ostream.h"

LLD_HAS_DRIVER(elf)
LLD_HAS_DRIVER(coff)
LLD_HAS_DRIVER(macho)
LLD_HAS_DRIVER(wasm)

extern "C" int xero_lld_main(int argc, const char **argv) {
  std::vector<const char *> args(argv, argv + argc);
  lld::Result r = lld::lldMain(args, llvm::outs(), llvm::errs(),
                               {{lld::WinLink, &lld::coff::link},
                                {lld::Gnu, &lld::elf::link},
                                {lld::Darwin, &lld::macho::link},
                                {lld::Wasm, &lld::wasm::link}});
  return r.retCode;
}

// lld's LTO code initializes every LLVM target; xeroc links only the ones
// Xero compiles for (x86, ARM, AArch64, RISC-V, WebAssembly), and never
// runs LTO, so the others are empty.
extern "C" {
void LLVMInitializeAMDGPUAsmParser() {}
void LLVMInitializeAMDGPUAsmPrinter() {}
void LLVMInitializeAMDGPUTarget() {}
void LLVMInitializeAMDGPUTargetInfo() {}
void LLVMInitializeAMDGPUTargetMC() {}
void LLVMInitializeAVRAsmParser() {}
void LLVMInitializeAVRAsmPrinter() {}
void LLVMInitializeAVRTarget() {}
void LLVMInitializeAVRTargetInfo() {}
void LLVMInitializeAVRTargetMC() {}
void LLVMInitializeBPFAsmParser() {}
void LLVMInitializeBPFAsmPrinter() {}
void LLVMInitializeBPFTarget() {}
void LLVMInitializeBPFTargetInfo() {}
void LLVMInitializeBPFTargetMC() {}
void LLVMInitializeHexagonAsmParser() {}
void LLVMInitializeHexagonAsmPrinter() {}
void LLVMInitializeHexagonTarget() {}
void LLVMInitializeHexagonTargetInfo() {}
void LLVMInitializeHexagonTargetMC() {}
void LLVMInitializeLanaiAsmParser() {}
void LLVMInitializeLanaiAsmPrinter() {}
void LLVMInitializeLanaiTarget() {}
void LLVMInitializeLanaiTargetInfo() {}
void LLVMInitializeLanaiTargetMC() {}
void LLVMInitializeLoongArchAsmParser() {}
void LLVMInitializeLoongArchAsmPrinter() {}
void LLVMInitializeLoongArchTarget() {}
void LLVMInitializeLoongArchTargetInfo() {}
void LLVMInitializeLoongArchTargetMC() {}
void LLVMInitializeM68kAsmParser() {}
void LLVMInitializeM68kAsmPrinter() {}
void LLVMInitializeM68kTarget() {}
void LLVMInitializeM68kTargetInfo() {}
void LLVMInitializeM68kTargetMC() {}
void LLVMInitializeMSP430AsmParser() {}
void LLVMInitializeMSP430AsmPrinter() {}
void LLVMInitializeMSP430Target() {}
void LLVMInitializeMSP430TargetInfo() {}
void LLVMInitializeMSP430TargetMC() {}
void LLVMInitializeMipsAsmParser() {}
void LLVMInitializeMipsAsmPrinter() {}
void LLVMInitializeMipsTarget() {}
void LLVMInitializeMipsTargetInfo() {}
void LLVMInitializeMipsTargetMC() {}
void LLVMInitializeNVPTXAsmPrinter() {}
void LLVMInitializeNVPTXTarget() {}
void LLVMInitializeNVPTXTargetInfo() {}
void LLVMInitializeNVPTXTargetMC() {}
void LLVMInitializePowerPCAsmParser() {}
void LLVMInitializePowerPCAsmPrinter() {}
void LLVMInitializePowerPCTarget() {}
void LLVMInitializePowerPCTargetInfo() {}
void LLVMInitializePowerPCTargetMC() {}
void LLVMInitializeSparcAsmParser() {}
void LLVMInitializeSparcAsmPrinter() {}
void LLVMInitializeSparcTarget() {}
void LLVMInitializeSparcTargetInfo() {}
void LLVMInitializeSparcTargetMC() {}
void LLVMInitializeSystemZAsmParser() {}
void LLVMInitializeSystemZAsmPrinter() {}
void LLVMInitializeSystemZTarget() {}
void LLVMInitializeSystemZTargetInfo() {}
void LLVMInitializeSystemZTargetMC() {}
void LLVMInitializeVEAsmParser() {}
void LLVMInitializeVEAsmPrinter() {}
void LLVMInitializeVETarget() {}
void LLVMInitializeVETargetInfo() {}
void LLVMInitializeVETargetMC() {}
void LLVMInitializeXCoreAsmPrinter() {}
void LLVMInitializeXCoreTarget() {}
void LLVMInitializeXCoreTargetInfo() {}
void LLVMInitializeXCoreTargetMC() {}
void LLVMInitializeXtensaAsmParser() {}
void LLVMInitializeXtensaTarget() {}
void LLVMInitializeXtensaTargetInfo() {}
void LLVMInitializeXtensaTargetMC() {}
}
