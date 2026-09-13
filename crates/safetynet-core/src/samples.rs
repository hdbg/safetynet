//! Instruction samples shared by the tests in this crate.

use crate::isa::*;

/// Every instruction, each carrying an operand this machine can actually run: a
/// frame displacement that stays inside a modest prologue, and a divisor that is
/// not zero.
///
/// Operands are asymmetric where they can be — `0xdead_beef` rather than
/// `0xabab_abab` — so a byte-order mistake shows up as a wrong value instead of
/// the same bytes read backwards.
pub(crate) fn instructions() -> Vec<Instr> {
    let frame = FrameSize::new(24).expect("24 is word-aligned");
    vec![
        Halt.into(),
        Push8 { imm: 0xa5 }.into(),
        Push32 { imm: 0xdead_beef }.into(),
        Push64 {
            imm: 0x0102_0304_0506_0708,
        }
        .into(),
        Drop.into(),
        Alloc { n: frame }.into(),
        Free { n: frame }.into(),
        Lds8 { disp: 1 }.into(),
        Lds32 { disp: 12 }.into(),
        Lds64 { disp: 8 }.into(),
        Sts8 { disp: 17 }.into(),
        Sts32 { disp: 20 }.into(),
        Sts64 { disp: 24 }.into(),
        Ld8.into(),
        Ld32.into(),
        Ld64.into(),
        St8.into(),
        St32.into(),
        St64.into(),
        Add.into(),
        Sub.into(),
        Mul.into(),
        Div.into(),
        Rem.into(),
        SDiv.into(),
        SRem.into(),
        And.into(),
        Or.into(),
        Xor.into(),
        BitNot.into(),
        Shl.into(),
        Shr.into(),
        Sar.into(),
        CmpEq.into(),
        CmpLt.into(),
        CmpLe.into(),
        CmpSLt.into(),
        CmpSLe.into(),
        Jmp { offset: 0 }.into(),
        Jz { offset: -12 }.into(),
        Jnz { offset: i32::MIN }.into(),
        Switch.into(),
        Host { index: 3 }.into(),
    ]
}
