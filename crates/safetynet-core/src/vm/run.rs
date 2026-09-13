//! The fetch loop: a program counter over encoded bytes, and the budget that
//! bounds it.

use super::{Flow, Trap, Vm};
use crate::encoding::{Decoder, Packed};
use crate::{ByteOrder, Instr};

impl<B: ByteOrder> Vm<B> {
    /// Runs `code` from its first byte until it halts, spending one unit of
    /// `fuel` per instruction, and hands the machine back.
    ///
    /// Reads the standard encoding; [`run_with`](Vm::run_with) takes a decoder
    /// for anything else.
    ///
    /// A program's result is the word on top of the stack when it halts, so
    /// [`pop`](Vm::pop) retrieves it. That convention lasts until marshalling
    /// exists, at which point `run` becomes generic over the result type and
    /// returns that instead.
    ///
    /// # Examples
    ///
    /// ```
    /// use safetynet_core::isa::{Add, Halt, Push8};
    /// use safetynet_core::{Le, Vm, encoding};
    ///
    /// let mut code = Vec::new();
    /// encoding::encode_all::<Le>(
    ///     &[
    ///         Push8 { imm: 2 }.into(),
    ///         Push8 { imm: 40 }.into(),
    ///         Add.into(),
    ///         Halt.into(),
    ///     ],
    ///     &mut code,
    /// )?;
    ///
    /// let vm = Vm::<Le>::new(vec![0; 64], 0).expect("a valid stack");
    /// let mut vm = vm.run(&code, 100)?;
    ///
    /// assert_eq!(vm.pop()?, 42);
    /// # Ok::<_, Box<dyn std::error::Error>>(())
    /// ```
    pub fn run(self, code: &[u8], fuel: u64) -> Result<Self, Trap> {
        self.run_with(code, &Packed::<B>::new(), fuel)
    }

    /// Runs `code` through `decoder`.
    ///
    /// The decoder's order is pinned to the machine's, so a program cannot be
    /// read in one order and executed in another. It must be the counterpart of
    /// whatever encoder laid the program out: this loop trusts the length it
    /// reports to find the next instruction, and a relative branch is measured
    /// against that same boundary.
    pub fn run_with<D: Decoder<Order = B>>(
        mut self,
        code: &[u8],
        decoder: &D,
        fuel: u64,
    ) -> Result<Self, Trap> {
        let mut fuel = fuel;
        let mut pc = 0;

        loop {
            let (instr, len) = fetch(code, pc, decoder)?;
            fuel = fuel.checked_sub(1).ok_or(Trap::OutOfFuel)?;

            // The offset a branch is measured from, and where execution
            // continues without one. Cannot overflow: it is at most `code.len()`.
            let next = pc + len;

            pc = match self.step(instr)? {
                Flow::Next => next,
                Flow::Jump(delta) => Self::target(code, pc, next, delta)?,
                Flow::Halt => return Ok(self),
            };
        }
    }

    /// Resolves a branch to the offset it lands on.
    ///
    /// The delta is measured from `next`, the first byte of the instruction that
    /// follows the branch, which is what keeps a fixed-width offset from
    /// depending on how long the branch itself encoded to.
    fn target(code: &[u8], branch: usize, next: usize, delta: i32) -> Result<usize, Trap> {
        let bad = || Trap::BadJump {
            from: branch,
            delta,
        };

        // Signed and wide, so that a target below zero is a rejected value
        // rather than a wrapped one.
        let target = i64::try_from(next)
            .map_err(|_| bad())?
            .checked_add(i64::from(delta))
            .ok_or_else(bad)?;

        let target = usize::try_from(target).map_err(|_| bad())?;
        if target >= code.len() {
            return Err(bad());
        }

        Ok(target)
    }
}

/// Decodes the instruction at `pc`.
fn fetch<D: Decoder>(code: &[u8], pc: usize, decoder: &D) -> Result<(Instr, usize), Trap> {
    let rest = code.get(pc..).ok_or(Trap::CodeOutOfRange { offset: pc })?;

    // Nothing left to decode means the last instruction was not a `HALT`; that
    // is a program that never stopped, not a malformed one.
    if rest.is_empty() {
        return Err(Trap::CodeOutOfRange { offset: pc });
    }

    decoder
        .decode(rest)
        .map_err(|_| Trap::BadInstruction { offset: pc })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::encoding::{encode_all, encoded_len};
    use crate::isa::*;
    use crate::{Be, Le, Word};

    fn assemble<B: ByteOrder>(program: &[Instr]) -> Vec<u8> {
        let mut code = Vec::new();
        encode_all::<B>(program, &mut code).expect("encodes");
        code
    }

    fn machine<B: ByteOrder>() -> Vm<B> {
        Vm::new(vec![0; 1024], 0).expect("a valid stack")
    }

    /// Runs a program and returns its result: the word left on top.
    fn eval<B: ByteOrder>(program: &[Instr]) -> Result<Word, Trap> {
        machine::<B>().run(&assemble::<B>(program), 1000)?.pop()
    }

    fn runs_a_program_to_halt<B: ByteOrder>() {
        let program = [
            Push8 { imm: 2 }.into(),
            Push8 { imm: 40 }.into(),
            Add.into(),
            Halt.into(),
        ];

        assert_eq!(eval::<B>(&program), Ok(42));
    }

    #[test]
    fn runs_a_program_to_halt_le() {
        runs_a_program_to_halt::<Le>();
    }

    #[test]
    fn runs_a_program_to_halt_be() {
        runs_a_program_to_halt::<Be>();
    }

    /// A backwards branch, resolved against real encoded sizes. `LDS64 8` is how
    /// a stack machine with no shuffle group spells `DUP`.
    fn a_backwards_branch_closes_a_loop<B: ByteOrder>() {
        // push8 3          counter
        // body:
        //   push8 1
        //   sub
        //   lds64 8        duplicate the counter
        //   jnz body
        // halt
        let body: [Instr; 4] = [
            Push8 { imm: 1 }.into(),
            Sub.into(),
            Lds64 { disp: 8 }.into(),
            Jnz { offset: 0 }.into(),
        ];

        let measure = |instr: &Instr| encoded_len(*instr).expect("measures");
        let entry = measure(&Push8 { imm: 3 }.into());
        let body_len: usize = body.iter().map(measure).sum();

        // The delta is measured from the first byte after the branch, which is
        // the end of the body.
        let delta = -i32::try_from(body_len).expect("a small body");
        assert_eq!(delta, -11, "2 + 1 + 3 + 5 bytes of loop body");
        assert_eq!(entry, 2);

        let mut program = vec![Push8 { imm: 3 }.into()];
        program.extend_from_slice(&body);
        program.push(Halt.into());
        let last = program.len() - 2;
        *program.get_mut(last).expect("the branch") = Jnz { offset: delta }.into();

        let mut vm = machine::<B>()
            .run(&assemble::<B>(&program), 1000)
            .expect("terminates");

        assert_eq!(vm.pop(), Ok(0), "the counter ran down");
        assert_eq!(vm.sp(), 0, "the loop is stack-neutral");
    }

    #[test]
    fn a_backwards_branch_closes_a_loop_le() {
        a_backwards_branch_closes_a_loop::<Le>();
    }

    #[test]
    fn a_backwards_branch_closes_a_loop_be() {
        a_backwards_branch_closes_a_loop::<Be>();
    }

    /// A jump onto itself is the smallest program that never ends, and fuel is
    /// the only reason this test terminates.
    #[test]
    fn fuel_bounds_a_program_that_never_stops() {
        let jmp = Jmp { offset: 0 }.into();
        let len = i32::try_from(encoded_len(jmp).expect("measures")).expect("small");
        let code = assemble::<Le>(&[Jmp { offset: -len }.into()]);

        assert_eq!(machine::<Le>().run(&code, 10).err(), Some(Trap::OutOfFuel));
    }

    /// Running past the last instruction is a program that never stopped, not a
    /// corrupt one.
    #[test]
    fn falling_off_the_end_traps() {
        let code = assemble::<Le>(&[Push8 { imm: 1 }.into()]);

        assert_eq!(
            machine::<Le>().run(&code, 10).err(),
            Some(Trap::CodeOutOfRange { offset: code.len() })
        );
    }

    #[test]
    fn undecodable_bytes_trap() {
        assert_eq!(
            machine::<Le>().run(&[0x7f], 10).err(),
            Some(Trap::BadInstruction { offset: 0 })
        );
    }

    #[test]
    fn a_branch_outside_the_code_traps() {
        let code = assemble::<Le>(&[Jmp { offset: i32::MIN }.into()]);

        assert_eq!(
            machine::<Le>().run(&code, 10).err(),
            Some(Trap::BadJump {
                from: 0,
                delta: i32::MIN
            })
        );

        // Forwards, onto the byte just past the end: still not an instruction.
        let code = assemble::<Le>(&[Jmp { offset: 0 }.into()]);
        assert!(machine::<Le>().run(&code, 10).is_err());
    }

    /// A trap raised by an instruction stops the loop where it happened.
    #[test]
    fn an_instructions_trap_propagates() {
        let program = [
            Push8 { imm: 1 }.into(),
            Push8 { imm: 0 }.into(),
            Div.into(),
            Halt.into(),
        ];

        assert_eq!(eval::<Le>(&program), Err(Trap::DivideByZero));
    }
}

#[cfg(test)]
mod format_tests {
    use super::*;
    use crate::Width;
    use crate::ir::{Cfg, Frame, Terminator, finalize_with};
    use crate::isa::{Add, Push8};
    use crate::samples::Padded;
    use crate::{Be, Le};

    /// A program laid out in one format and read back in the same one, with the
    /// machine never learning which format that was.
    ///
    /// The two halves have to agree about lengths or nothing works: the loop
    /// finds each instruction at the boundary the previous one reported, and
    /// every branch offset was resolved against those same boundaries.
    fn another_format_runs_end_to_end<B: ByteOrder>() {
        let mut frame = Frame::new();
        let total = frame.add(Width::U64).expect("room");

        let mut builder = Cfg::builder(frame);
        let entry = builder.block(0);
        let body = builder.block(0);

        builder
            .at(entry)
            .expect("open")
            .instr(Push8 { imm: 40 })
            .store(total);
        builder.seal(entry, Terminator::Jmp(body)).expect("seals");

        builder
            .at(body)
            .expect("open")
            .load(total)
            .instr(Push8 { imm: 2 })
            .instr(Add);
        builder.seal(body, Terminator::Halt).expect("seals");

        let cfg = builder.build(entry).expect("builds");
        let format = Padded::<B>::default();
        let program = finalize_with(&cfg, &format).expect("finalizes");

        let packed = crate::ir::finalize::<B>(&cfg).expect("finalizes");
        assert!(
            program.code().len() > packed.code().len(),
            "the padding is really there"
        );

        let vm = Vm::<B>::new(vec![0; 256], 0).expect("a valid stack");
        let mut vm = vm
            .run_with(program.code(), &format, 1000)
            .expect("terminates");

        assert_eq!(vm.pop(), Ok(42));
    }

    #[test]
    fn another_format_runs_end_to_end_le() {
        another_format_runs_end_to_end::<Le>();
    }

    #[test]
    fn another_format_runs_end_to_end_be() {
        another_format_runs_end_to_end::<Be>();
    }

    /// The standard encoding is not special to the machine: reading a padded
    /// program with the standard decoder walks into the filler and stops.
    #[test]
    fn the_wrong_decoder_does_not_quietly_work() {
        let mut builder = Cfg::builder(Frame::new());
        let entry = builder.block(0);
        builder.at(entry).expect("open").instr(Push8 { imm: 7 });
        builder.seal(entry, Terminator::Halt).expect("seals");

        let cfg = builder.build(entry).expect("builds");
        let program = finalize_with(&cfg, &Padded::<Le>::default()).expect("finalizes");

        let vm = Vm::<Le>::new(vec![0; 256], 0).expect("a valid stack");
        assert_eq!(
            vm.run(program.code(), 1000).err(),
            Some(Trap::BadInstruction { offset: 2 }),
            "the filler after the first instruction is not an opcode"
        );
    }
}
