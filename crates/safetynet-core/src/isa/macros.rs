//! The `define_ops!` generator.
//!
//! Every instruction is declared once, in the table in the parent module, and
//! this macro turns each declaration into the operand struct, its [`Op`]
//! implementation, and its arm in the [`Instr`] enum.
//!
//! [`Op`]: super::Op
//! [`Instr`]: super::Instr

/// Emits one operand struct.
///
/// Two arms because a unit struct and a braced struct differ in their trailing
/// token — `struct Add;` versus `struct Push8 { .. }` — and one arm cannot
/// produce both without a stray semicolon.
macro_rules! define_op_struct {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Encode, Decode)]
        #[musli(packed)]
        pub struct $name;
    };
    ($(#[$meta:meta])* $name:ident { $($(#[$fmeta:meta])* $field:ident : $fty:ty),* $(,)? }) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Encode, Decode)]
        #[musli(packed)]
        pub struct $name {
            $($(#[$fmeta])* pub $field: $fty),*
        }
    };
}

/// Declares the instruction set.
///
/// Each entry reads:
///
/// ```text
/// /// Documentation for the instruction.
/// Push32 { /// Documentation for the operand.
///          imm: u32 }
///     = "push32", sp(|_| WORD), exec(|vm, op| vm.push_imm(op.imm.into()));
/// ```
///
/// `sp(..)` and `exec(..)` *look* like closures but are not: the `|..|` heads
/// bind names for the operand struct and the machine, and the body is spliced
/// straight into the generated method. They are written this way because a
/// `macro_rules!` body cannot refer to the `self` of a method it generates —
/// `self` is hygienic, and one written in the table would not resolve to the one
/// in `sp_delta` or `exec`. Naming the operand at the table sidesteps that, and
/// is what lets `ALLOC`'s stack effect depend on its own operand, which a
/// constant could not express.
///
/// Since the bodies are spliced in rather than called, an `exec` body may also
/// name the byte order `B` even though nothing in the table declares it: generic
/// parameters are not hygienic, unlike `self`.
///
/// Declaring behaviour beside the stack effect is the point of the table. An op
/// that forgets either one does not compile, where a central table keyed by
/// opcode number would simply have a missing entry.
macro_rules! define_ops {
    ($(
        $(#[$meta:meta])*
        $name:ident
            $({ $($(#[$fmeta:meta])* $field:ident : $fty:ty),* $(,)? })?
            = $mnemonic:literal,
              sp(|$sp_op:pat_param| $sp:expr),
              exec(|$vm:pat_param, $exec_op:pat_param| $exec:expr);
    )*) => {
        $( define_op_struct!($(#[$meta])* $name $({ $($(#[$fmeta])* $field: $fty),* })?); )*

        $(
            impl Op for $name {
                const MNEMONIC: &'static str = $mnemonic;

                fn sp_delta(&self) -> i32 {
                    let $sp_op: &Self = self;
                    $sp
                }

                fn exec<B: ByteOrder>(&self, vm: &mut Vm<B>) -> Result<Flow, Trap> {
                    let $vm: &mut Vm<B> = vm;
                    let $exec_op: &Self = self;
                    $exec
                }
            }

            impl From<$name> for Instr {
                fn from(op: $name) -> Self {
                    Self::$name(op)
                }
            }
        )*

        /// One instruction: an operation together with its operands.
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Encode, Decode)]
        pub enum Instr {
            $(
                $(#[$meta])*
                #[musli(transparent)]
                $name($name),
            )*
        }

        impl Instr {
            /// The assembly mnemonic for this instruction.
            pub const fn mnemonic(self) -> &'static str {
                match self { $( Self::$name(_) => $mnemonic, )* }
            }

            /// Net change this instruction makes to `SP`, in bytes.
            ///
            /// Positive grows the stack. This is the number the CFG validator
            /// folds over a block, and the number every frame displacement is
            /// ultimately computed from.
            pub fn sp_delta(self) -> i32 {
                match self { $( Self::$name(op) => op.sp_delta(), )* }
            }

            /// Executes this instruction on `vm`.
            pub fn exec<B: ByteOrder>(self, vm: &mut Vm<B>) -> Result<Flow, Trap> {
                match self { $( Self::$name(op) => op.exec(vm), )* }
            }
        }
    };
}

pub(crate) use {define_op_struct, define_ops};
