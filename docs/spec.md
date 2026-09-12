# safetynet — Specification

A Rust-embedded bytecode VM for CTF reversing challenges. A complexity-limited
Rust function is annotated, lowered to an intermediate representation, compiled
to bytecode, and its body replaced with a call into the VM interpreter. To a
reverser the shipped artifact is a small stack machine plus an opaque byte
image; to the author it is an ordinary Rust function with a test proving the two
agree.

This is a single consolidated spec. The final section is a critical review of
it — read that before committing to the design, because several of its findings
change decisions made earlier in the document.

---

## 1. Goals, non-goals, threat model

**Goals.** The author writes normal Rust; the VM is invisible at the call site.
Arbitrary user structs and enums work as inputs and return values with correct
field offsets. The type system rejects non-representable locals at compile time
with a pointing error. Every build is validated against the original Rust.

**Non-goals (now).** No recursion, no heap, no generics/traits/closures/floats
in guest code — the restriction is on the guest; the host-side machinery does use
generics, notably for byte order (§3). No mutation/obfuscation passes yet — the
IR is structured so they can be added later. No guest-supplied bytecode.

**Threat model.** Reversing, not sandbox escape. The bytecode and memory image
are baked at build time; the adversary reads the binary and may instrument the
interpreter, but does not supply their own bytecode. Input *data*, however, does
cross an untrusted boundary (§6), which matters more than it first appears
(see Review §R2).

---

## 2. Workspace

Proc-macro crates may only export macros, so all reusable logic lives in a
library crate that both the macros and the runtime depend on. This is also what
prevents the encoder and decoder from drifting — the bug class that presents as
a VM fault but is really an encoding mismatch.

```
safetynet-core     opcode table (define_ops!), IR types, traits, byte-order
                   policy, layout descriptors, assembler, interpreter
safetynet-macros   proc-macros: #[safetynet], #[derive(VmLayout)], sn_asm!
                   (thin; each builds a Cfg and calls core's pipeline)
safetynet          façade re-exporting core + macros
```

---

## 3. Machine model and instruction set

A **stack machine** — expression lowering is a post-order walk, opcodes carry at
most an immediate, and the encoding is compact. No registers: the
register-vs-stack question resolves to stack because the macro-side effort is
nearly identical while the stack wins on total code (encoder, interpreter, and
the two-cell handling of slices).

### 3.1 One address space, one stack pointer

There is **one flat `Vec<u8>` address space** (§6.1) and one machine register,
`SP`. The stack is a region of that space, it grows **upward**, and `SP` is the
byte index one past the top. There is no locals array: locals, temporaries and
frame-resident aggregates are all just bytes below `SP`.

- **The operand stack is word-granular.** Every push and pop moves one 8-byte
  word, so `SP` stays 8-aligned. `PUSH` writes a word; `ADD` pops two, pushes one.
- **The frame is byte-granular.** `ALLOC n` (n rounded up to a multiple of 8)
  reserves the frame in the prologue; each local is a **cell** at a byte offset
  inside it, sized to its logical width (1, 4 or 8 bytes). `FREE n` releases it.
- **Access is SP-relative, by byte displacement.** `LDS.w k` pushes the `w`-wide
  value at `SP − k` zero-extended to a word; `STS.w k` pops a word and writes its
  low `w` bytes to `SP − k`. A cell's bytes are laid out in the build's byte order
  `B` (§3.2). The stack's *growth direction* is fixed and is not the byte order —
  the two are independent and conflating them is the classic bug here.

**Displacements are computed, never guessed.** For a frame of size `F`, a cell at
frame offset `c`, and `d` bytes of operand stack pushed since the prologue:

```
k = F − c + d          (displacement of cell c at this program point)
```

`F` and `c` come from the frame layout; `d` is the operand-stack depth the IR
already tracks at every program point (§8). The objection that originally ruled
out depth-relative addressing — *every intervening temporary shifts a local's
position* — is answered by making the shift a compile-time constant rather than
by avoiding it. **The price is real: the SP invariant stops being a validation
nicety and becomes load-bearing for correctness.** A tracking bug used to mean a
stack runaway the validator would catch loudly; now it means a silent read of the
wrong cell (Review §R9).

**The IR contains no displacements.** Local access stays symbolic — `Local(cell)`
— through the mutation seam, and is materialized into `LDS.w k` / `STS.w k`
during finalization, the pass that already knows `SP` at every point. Mutation
passes can therefore still insert and delete stack traffic without touching a
single offset (§14); had displacements been baked at lowering time, every junk
push would have invalidated every later access in the block.

**Consequence: the shuffle group mostly disappears.** `DUP` is `LDS.u64 8`,
`OVER` is `LDS.u64 16`, `SWAP` is a pair of `LDS`/`STS` (or, as the lowerer
usually prefers, a frame temp). Only `DROP` survives as its own opcode, being a
spelling of `FREE 8` common enough to earn one.

### 3.2 Byte order

**Byte order is a compile-time type parameter, not a constant.** The machine is
`Vm<B: ByteOrder>`, and the assembler, the image builder and `marshal`/
`unmarshal` take the same `B`. `ByteOrder` is a sealed trait with two ZST
implementors:

```rust
pub trait ByteOrder: sealed::Sealed + Copy + Clone + Debug + 'static {
    const NAME: &'static str;                      // "le" / "be" — diagnostics only
    fn read_u32(bytes: [u8; 4]) -> u32;    fn write_u32(value: u32) -> [u8; 4];
    fn read_u64(bytes: [u8; 8]) -> u64;    fn write_u64(value: u64) -> [u8; 8];
}
pub struct Le;
pub struct Be;
```

Everything that touches bytes is generic over `B` and monomorphized: no runtime
branch, no `ByteOrder` value in the binary, just the one order's shift/or
sequence inlined at each site.

**The order is named literally at the source, never by cargo feature.** `Le` is
the compiled-in default; anything else is written at the site —
`#[safetynet(order = Be)]`, `sn_asm!(Be { … })`. `safetynet::Order` exists as an
alias for the default so signatures can spell it, but it is documentation, not
configuration: the macro bakes `.rodata` bytes at expansion time and therefore
needs a *concrete* order in hand, so it resolves the default itself rather than
deferring to a type alias that only const-eval would see.

Features were considered and rejected. A proc-macro cannot read the features of
another crate at all, and can read its own only if the façade forwards them
deliberately; even done correctly, cargo features are global and additive, so two
crates in one build graph could not choose different orders — the knob would be
per-*graph*, which is not what "per program" means. A literal at the call site is
both more honest and more local.

Because a single type parameter is threaded from the `#[safetynet]` expansion
through the image builder, the interpreter, every frame cell and both marshal
directions, guest access and host marshalling *cannot* disagree: a mismatch is a
type error at the `Vm<B>::run(&Program<B>)` call, not a wrong answer. This
promotes Review §R8's first bullet from a comment to a rule. Since locals now
live in byte memory, `B` is observable for them too, not only at the `.input`/
`.ret` boundary. A non-`Le` build is also mildly hostile to a reverser reaching
for the obvious `u32::from_le_bytes` hypothesis — a free side effect, not a
security argument.

### 3.3 Instruction set

- Arithmetic: `ADD SUB MUL DIV REM SDIV SREM`. `DIV`/`REM` are unsigned;
  `SDIV`/`SREM` interpret both operands as `i64`. All four trap on a zero
  divisor, and the signed pair additionally traps on `i64::MIN / -1`, the one
  signed division with no representable result.
- Bitwise: `AND OR XOR NOT SHL SHR SAR`. `SHR` is logical, `SAR` arithmetic. The
  shift count is **masked to its low 6 bits**, so a shift by 64 is a shift by 0;
  leaving that undefined is how a VM and its reference come to disagree on the
  one input nobody tested.
- Compare: `EQ LT LE SLT SLE` → a full word, 0 or 1. `LT`/`LE` are unsigned,
  `SLT`/`SLE` signed; the remaining orderings come from operand swap.
- Frame: `ALLOC n`, `FREE n`, `LDS8 LDS32 LDS64 k`, `STS8 STS32 STS64 k` (`k` a
  byte displacement back from `SP`). Loads zero-extend to a word; stores write
  the low bytes.
- Stack: `PUSH8 PUSH32 PUSH64 imm` (zero-extended to a word), `DROP`
- Memory: `LD8 LD32 LD64` / `ST8 ST32 ST64` at an absolute address popped from
  the stack; multi-byte accesses use the build's byte order `B`, `LD8`/`ST8` are
  order-independent
- Control: `JMP`, `JZ`, `JNZ` (relative offsets), `SWITCH` (jump table)
- `HOST k` (host escape, §7.4), `HALT`

`SWITCH` and `HOST` have **reserved opcodes that trap on execution** until the
subset needs them — `match` lowering and the host table respectively. Reserving
the bytes now keeps opcode numbering stable across the phases that add them.

**Encoding.** One byte of opcode, then the immediate if there is one, in the
build's byte order `B`:

| immediate | width | notes |
|---|---|---|
| `PUSH8` / `PUSH32` / `PUSH64` | 1 / 4 / 8 | zero-extended to a word |
| `ALLOC` / `FREE` | `u16` | must be a multiple of 8; other values fail to decode |
| `LDS*` / `STS*` | `u16` | displacement back from `SP` |
| `JMP` / `JZ` / `JNZ` | `i32` | relative to the first byte of the *next* instruction |
| `HOST` | `u8` | host table index |

Widths are fixed per opcode, never variable-length. `PUSH8` exists so small
constants stay cheap, but nothing is a varint, because a varint makes an
instruction's encoded size depend on its operand — which is exactly what block
reordering and opcode renumbering must not have to reason about. The `i32`
branch offset is deliberately roomy: with calls inlined and no code-size limit
yet pinned (§R6), an `i16` would impose a hard failure at an arbitrary
boundary.

Relative branch offsets keep code position-independent. There is no `CALL`/`RET`:
internal calls are inlined at lowering time (§7.5), so a frame is per *compiled
program*, not per call — and nothing on the stack is a return address, which is
why the guest addressing its own stack through `LD*`/`ST*` is harmless.

**Word type is `u64`, monomorphic, wrapping** — for arithmetic and compares.
Only the memory and frame ops carry a width. Narrow cells make truncation free
for `+ − × <<` (a store into a `u8` cell *is* the mask) but not for `/ % >>`,
comparisons, or signed operations; Review §R1 is partially paid by this change
and the residue is stated there.

### 3.4 Opcodes are types, not enum arms with a lookup table

Each opcode is a **struct** implementing `Op`; `Instr` is the enum over those
structs. The trait exists so that stack tracking is never hardcoded: the
validator asks each instruction for its own effect on `SP` instead of consulting
a `match` that a newly added opcode can silently fall out of.

```rust
pub trait Op: Copy + Sized {
    const OPCODE:   u8;
    const MNEMONIC: &'static str;
    /// Net change to SP, in bytes. Takes &self because ALLOC/FREE's effect
    /// *is* their immediate.
    fn sp_delta(&self) -> i32;
    /// Encoded length, immediate included.
    fn size(&self) -> usize;
    fn encode<B: ByteOrder>(&self, out: &mut Vec<u8>);
    fn decode<B: ByteOrder>(src: &[u8]) -> Result<Self, DecodeErr>;
    fn exec<B: ByteOrder>(&self, vm: &mut Vm<B>) -> Result<Flow, Trap>;
}

struct Add;                       // sp_delta = -8
struct Push32 { imm: u32 }        // sp_delta = +8
struct Alloc  { n: u16 }          // sp_delta = +(n as i32)   ← immediate-dependent
struct Lds32  { disp: u16 }       // sp_delta = +8
struct Sts32  { disp: u16 }       // sp_delta = -8
```

Access width is part of the *opcode*, not a field: `Lds8`/`Lds32`/`Lds64` are
three types, because `OPCODE` is an associated const and one type cannot carry
three of them. It also keeps decoding a fixed-width read per opcode.

Every op is declared once, in a `define_ops!` table that generates the structs,
the `Instr` enum and its forwarding `impl Op`, the decode dispatch, and the
mnemonic table shared by `sn_asm!` (§8.1) and the disassembler:

```rust
define_ops! {
    0x01 Push  { imm: Word } "push"  sp(+8)      exec |vm, op| vm.push(op.imm),
    0x02 Alloc { n: u16 }    "alloc" sp(+n)      exec |vm, op| vm.sp_add(op.n)?,
    0x03 Lds { w: Width, disp: u16 } "lds" sp(+8)
         exec |vm, op| { let v = vm.read_rel::<B>(op.disp, op.w)?; vm.push(v) },
    0x10 Add                 "add"   sp(-8)
         exec |vm, _| { let (a, b) = vm.pop2()?; vm.push(a.wrapping_add(b)) },
    // …
}
```

One declaration site means encoder, decoder, SP model, interpreter, assembler and
disassembler cannot drift — the bug class §2 exists to prevent, closed by
construction instead of by discipline. It also means the SP model is *total*:
`sp_delta` is a trait method, so an op that forgets to declare its effect does
not compile, whereas a missing arm in a central `match` is a runtime surprise.
`Terminator`s implement the same trait (`Br` pops a word, `Jmp` is neutral), so
block-boundary arithmetic uses one mechanism throughout.

---

## 4. The two traits

The input-aggregate machinery and the local-eligibility machinery are separate
concerns, so they are separate traits.

**`VmValue` — what may be a local or an operand.** A type that fits one VM word.
Sealed; blanket-implemented only for the supported scalar(s). This gates locals.

```rust
pub trait VmValue: sealed::Sealed + Copy {
    fn to_word(self) -> Word;      // Word = u64
    fn from_word(w: Word) -> Self;
}
```

The lowerer cannot type-check the body (it sees tokens, not resolved types), so
it emits, per local, a bound assertion in dead code — a non-`VmValue` local then
fails in the type checker with an error pointing at the binding.

**`VmLayout` + `#[derive(VmLayout)]` — aggregates in memory.** Structs and
data-carrying enums used as inputs/returns. Defines the *canonical flat layout*,
the single source of truth for offset resolution.

```rust
pub trait VmLayout {
    const LAYOUT: &'static Layout;                        // order-independent
    const SIZE: usize;                                    // order-independent
    fn marshal<B: ByteOrder>(&self, mem: &mut [u8]);      // host  -> VM image
    fn unmarshal<B: ByteOrder>(mem: &[u8]) -> Self;       // VM image -> host
}
struct Layout { fields: &'static [Field] }
struct Field  { name: &'static str, offset: u32, size: u32, nested: Option<&'static Layout> }
```

The derive computes offsets from the VM's own packing rules, **not** from Rust's
`#[repr]`, which is unspecified. Because `marshal` and guest field loads both
read `LAYOUT`, they cannot disagree *within a coherent build* (Review §R8 notes
the caveat).

Offsets are a packing question and byte order is a representation question, so
they are separated: `LAYOUT`/`SIZE` carry no `B` and stay usable from `const fn`
(§10), while only the two byte-moving methods are generic. The derive therefore
emits one generic body per type, not one impl per order, and a nested aggregate
forwards the caller's `B` unchanged. `VmValue`'s `to_word`/`from_word` are
word-level and likewise order-free — bytes only acquire an order when they enter
the image.

---

## 5. Type model in the macro

Every local carries a compile-time shadow type. A local *is* a frame cell — a
byte offset and a width — never a slot index:

```rust
enum Ty {
    Scalar    { cell: Cell },                     // one cell, width 1/4/8
    Slice     { ptr: Cell, len: Cell },           // two word cells
    Aggregate { base: FrameOff, layout: LayoutRef },  // laid out in the frame
    AggRef    { cell: Cell, layout: LayoutRef },      // cell holds an absolute address
}
struct Cell { off: u32, width: Width }            // byte offset within the frame
```

Scalars and slices are cells. An aggregate either lives **directly in the frame**
— `base` is its frame offset and field access is `base + field.offset`, the same
`LAYOUT` at a different base — or is reached through a cell holding an absolute
address, which is how an `.input` aggregate is read without copying it. The frame
being byte-granular is what makes this work: frame layout and canonical aggregate
layout are the same packing discipline, so there is no second set of rules for
locals.

Cell assignment is a frame-layout pass in the lowerer: one cell per local, widths
from the shadow type, frame size rounded to 8. Cells are never reused across
locals for now; overlapping the cells of locals with disjoint lifetimes is a
later optimization that the symbolic `Local(cell)` form leaves open.
Out-of-subset constructs are rejected with `syn::Error::new_spanned`.

---

## 6. Memory model and offset resolution

### 6.1 Image map

```
.rodata    literals, const tables, S-boxes                (immutable image)
.input     marshalled input aggregate(s), per VmLayout         (UNTRUSTED)
.ret       return slot / marshalled return aggregate
.scratch   work area, output buffers
.stack     frame + operand stack, grows upward; SP bound-checked to its end
```

**The order of the regions is fixed; the addresses are not.** Bases are computed
per program from the sizes the build actually needs and recorded in
`Program<B>`, which is what the interpreter bounds-checks against. Hard-coding
`.rodata` at `0x0000` and `.input` at `0x0800` would cap a const table at 2 KiB
for no reason and pad every small program to the same size; a computed layout
costs one struct of `u32`s and removes a whole class of "the S-box grew" bug.
Sizes are known at finalization, so nothing about this is dynamic.

One address space, not two: `.stack` is a region like any other, `LD*`/`ST*` can
address it with an absolute address, and `LDS`/`STS` are the SP-relative form of
the same access (§3.1). Nothing on the stack is a return address — there are no
calls — so the aliasing is harmless, and the interpreter gets one bounds check
instead of two.

The host marshals inputs into `.input`, runs to `HALT`, then reads `.ret` via
`unmarshal`. Everything crossing the boundary is `VmValue`/`VmLayout`. The image
is built by `Image<B>` and consumed by `Vm<B>` with the same `B` that both
marshal directions use, so the boundary has exactly one byte order by
construction.

### 6.2 Deferred resolution — the central mechanism

`#[safetynet]` and `#[derive(VmLayout)]` are independent invocations. When the
lowerer sees `input.seq` it has the field *name* but not the offset — that
belongs to the derive on another type. Neither macro can read the other's
computed values at expansion time.

So the lowerer emits a *symbolic*, name-keyed reference:

```rust
Operand::Field { root: LocalId, path: &["header", "seq"] }
```

Offsets and load widths are filled in during finalization from
`<T as VmLayout>::LAYOUT`. The trait is the interface across which the two macros
communicate, deferred to a phase where the type is fully known. (Review §R4
argues this deferral is also the spec's main structural liability.)

---

## 7. Supported Rust subset

The unifying rule: everything data-carrying is a **tag plus a memory-resident
payload under a VM-canonical layout with no niche optimization**, and the Rust
compiler validates on the hidden reference (§10) everything the macro declines
to prove — exhaustiveness, arm types, conversions.

### 7.1 Control flow
`if`/`else`, `while`, `loop`, `for _ in a..b`. `break`/`continue` via a
loop-context stack (`continue` in a `for` targets the increment block, not the
head). `&&`/`||` short-circuit into branches, never bitwise. `return` jumps to a
shared epilogue.

### 7.2 Enums
Field-less enums (`#[repr(u8)]`) are a discriminant that fits a word — a
`VmValue` via `#[derive(VmValue)]`, free. Data-carrying enums are a struct with a
tag: `tag` followed by a payload region sized to the largest variant, each
variant's fields at known offsets. Explicit tag, no niche tricks.

### 7.3 match
Dense discriminants lower to `SWITCH`; sparse or guarded matches degrade to a
compare-and-branch chain. Supported patterns: literals, wildcards, unit variants,
single-level data-variant patterns with bindings, or-patterns, ranges, guards
(a failed guard falls through to the next candidate). **Rejected** with a spanned
error: nested/structural destructuring, `@`-bindings. Exhaustiveness is left to
the reference (Review §R2 shows why that is not sufficient).

### 7.4 Result and `?`
`Result<T, E>` is a two-variant data enum. `?` lowers to a tag-branch: on `Err`,
store into `.ret` and jump to the epilogue. Restricted to a **single fixed error
type** — `From`-converting `?` is rejected, since cross-type conversion is
generic trait dispatch the subset cannot do.

### 7.5 Calls
Calls to other annotated functions are **inlined** at lowering time (no
`CALL`/`RET`, bounded by no-recursion). A proc-macro sees only the item it is
attached to, so "inlined" requires the callee to be *visible to the same
invocation*, which means applying `#[safetynet]` to the enclosing **module**.
A call to an annotated function outside the current invocation (hosts aside) is
rejected with a spanned error telling the author to widen the attribute's scope.
This is the same cross-macro blindness that forced §6.2's deferred field
resolution, and unlike offsets it has no `LAYOUT`-style escape — a callee's body
is not reachable through a trait const. Calls to the **host** go through a closed,
author-registered table indexed by `HOST k`, recognized via `#[safetynet::host]`,
args/return marshalled through `.args`/`.ret`. Arbitrary std/library calls are
rejected. A `HOST k` is a labeled signpost in the disassembly — for I/O, not for
hiding the crypto you want reversed.

---

## 8. Intermediate representation

A control-flow graph. Structured control flow is gone; byte offsets do not exist
yet, which is what makes offset resolution and (future) mutation clean.

```rust
struct Cfg   { entry: BlockId, blocks: Vec<Block>, frame: Frame }
struct Frame { size: u32, cells: Vec<Cell> }              // byte offsets + widths
struct Block { id: BlockId, sp_in: u32, code: Vec<Instr>, term: Terminator }
enum Terminator {
    Jmp(BlockId),
    Br { then: BlockId, els: BlockId },
    Switch { arms: Vec<BlockId>, default: BlockId },      // default mandatory, §R2
    Ret,
    Halt,
}
```

Each block is straight-line and ends in exactly one terminator; edges are
`BlockId`s. `sp_in` is the operand-stack depth **in bytes, above the frame** on
entry to the block — recorded by the assembler as it parses and checked by the
validator, so it is an assertion rather than a datum to be trusted. Local
access inside `code` is symbolic (`Local(cell)`), not an `LDS`/`STS` with a
displacement; §3.1 explains why, and finalization is where the two meet.

This is one IR plus a flat encoding — not a two-tier MIR/LIR; there is no
register allocation or instruction selection for a second level to do. A flat
instruction-stream tier is introduced only if encoding-level mutation later
demands it.

**SP invariant (the IR's oracle, and now part of correctness).** The validator
folds `sp_delta()` over each block — asking the instructions, never a table —
and asserts that every predecessor of a block agrees on its `sp_in`, that the
recorded `sp_in` matches, that no point inside a block has negative depth, and
that the frame is intact at every terminator. Two properties follow. First, `SP`
is a *static function of the program point*, which is what makes byte-displacement
addressing decidable at compile time at all. Second, because `sp_delta` is a
trait method on the op (§3.4) rather than a central `match`, an op cannot be
added without declaring its effect. The check runs inside the macro, so a
lowering mistake is a spanned error rather than a wrong answer — which matters
more than it did when locals were absolutely indexed, because the failure mode
has changed from a runaway to a silent misread (§R9).

### 8.1 `sn_asm!` — textual front-end to the CFG

`sn_asm!` is a proc-macro that takes assembly-like source with **jump labels**,
resolves it to a `Cfg`, validates it, and finalizes it to a `Program<B>`. It
computes no byte offsets of its own: labels become `BlockId` edges, and offsets
appear only where they already appeared, in finalization.

It is a *front end*, not the backend. The lowerer (§9) builds a `Cfg` by calling
the same `core` API this macro calls; neither goes through the other. What keeps
the two honest is not a shared code path but a **shared property**, and it is
normative:

```
parse(print(cfg)) == cfg        for every fixture, both orders
```

`print` is the disassembler and `parse` is this macro's front half, both of which
have to exist anyway (§R8's round-trip). Running the property over the lowerer's
own output means an asm surface that cannot express something the IR can is a
*failing test*, not a discovery made a year later by someone trying to hand-write
it. That is the dogfooding guarantee, bought without putting a parser on the
production path (§R10).

```rust
const PROG: Program<Le> = sn_asm!(Le {
    .frame   i: u32, acc: u8          // cells; the macro assigns byte offsets
    .rodata  KEY: [u8; 5] = [0x1f, 0x8b, 0x00, 0x5a, 0xc3]

entry:
    enter                   // ALLOC <frame size, rounded to 8>, zeroed
    push 0
    store acc               // STS.u8  — truncates to the cell's width
    push 0
    store i
head:
    load  i                 // LDS.u32 — zero-extends to a word
    push  5
    lt
    jz done                 // conditional; the other arm is fallthrough → body
body:
    load  acc
    push  KEY               // .rodata symbol → base address
    load  i
    add
    ld8
    xor
    store acc
    load  i
    push  1
    add
    store i
    jmp head
done:
    push  .ret
    load  acc
    st8                     // result into .ret
    leave                   // FREE <frame size>
    halt
});
```

Rules:

- **A label opens a block; a terminator closes it.** Fallthrough into the next
  label is materialized as an explicit `Jmp`, so every `Block` ends in exactly one
  `Terminator` as §8 requires. Straight-line code that neither branches nor falls
  into a label is a spanned error, not silent dead code.
- **Conditionals name one target; the other is fallthrough.** `jz L` becomes
  `Br { then: L, els: <next block> }` — `jz` takes the branch when the top of
  stack is zero — and `jnz` is the same with the arms swapped. `switch` takes a
  label list and a **mandatory** `default:` arm, which makes Review §R2's trapping
  default a syntactic requirement rather than a convention.
- **Symbols, never numbers.** Cells are named in `.frame` with widths and become
  `Local(cell)` references; `.rodata` items are named and left for the finalizer
  that places them to turn into base addresses; branch targets are labels. Nothing
  in the source is a byte offset — which is precisely what makes the macro a
  trustworthy test of the backpatching it does not perform.
- **`load`/`store` are the whole point.** Each expands to `LDS.w k` / `STS.w k`
  with `k = F − c + d` computed from the SP the macro is tracking at that exact
  line (§3.1). Hand-writing those displacements is not realistic — one inserted
  `push` shifts every subsequent access in the block — so SP-relative locals make
  a symbolic assembler a requirement rather than a convenience. Raw `lds`/`sts`
  with literal displacements stay spellable, for tests that target the
  materialization itself.
- **Field holes work here too.** `field Packet::header.seq` emits the same
  symbolic `Operand::Field` the lowerer emits (§6.2), so the relocation path can be
  exercised before either the lowerer or the derive exists.
- **Validation is the macro's job.** SP invariant, single terminator, edge
  consistency, unknown or duplicate label, unknown cell, wrong arity — every
  failure is a `syn::Error` on the offending token at expansion time. `sn_asm!`
  runs entirely proc-macro-side, so like the rest of the assembler (§10) it is
  categorically absent from the target binary.
- **Byte order is the macro's first argument** and becomes the `B` of the emitted
  `Program<B>`; omitting it uses `safetynet::Order`. `Vm<Be>` will not run a
  `Program<Le>`.
- *Note:* the example writes `push k` for an immediate constant, and §3's list has
  no such opcode — an omission there, not a decision here. Whichever form Phase 1
  settles on (a `PUSH8/PUSH32/PUSH64` family, or a `.rodata` load), the mnemonic is
  the assembler's stable surface.

Why this earns a phase of its own, ahead of the lowerer:

1. **It replaces the throwaway.** Phase 1's hand-assembler is deleted by design;
   this is its permanent successor, so the interpreter, the CFG builder and the
   finalizer keep a working front-end that does not wait on the Rust subset.
2. **It makes the IR legible.** `print`/`parse` turn a `Cfg` into text a human can
   review, so a lowering test can assert against readable asm and a bug can be
   bisected by hand-writing the expected program. `SN_DUMP_ASM=1` dumps what an
   annotated function lowered to, which is the debugger the compiler otherwise
   lacks.
3. **It closes the round-trip gap.** `parse → Cfg → encode → decode → print →
   parse` must reach a fixed point — Review §R8's missing property test, for both
   orders — and the same printer/parser pair is what enforces surface totality
   above.
4. **It is where hand-written guest routines go** when the Rust subset cannot
   express something, instead of widening the subset to accommodate it.

---

## 9. Compilation pipeline

Two front-ends, one backend — and the backend is a library, not a macro:

```
 syn::ItemFn                             sn_asm! source (§8.1)
   │  parse + subset check               │  parse; labels → BlockId edges
   │  (spanned errors)                   │  (spanned errors)
   ▼                                     ▼
 lower → IR (CFG)  ◄──────────────────────┘   frame layout; symbolic
   │                                         cells, fields, consts;
   │                                         structured CF → branches
   │  validate (SP invariant, single-terminator, edge consistency, limits §R6)
   ▼
 [ mutation passes ]       ← omitted now; the seam lives exactly here
   ▼
 finalize<B>:              materialize Local(cell) → LDS/STS displacements from
   │                       the tracked SP; resolve Field paths via LAYOUT; lay
   │                       out blocks; BlockId → offsets; image in order B
   ▼
 bytecode + memory image   → Program<B>
```

Block layout is chosen here; the backpatching becomes this single pass, and `B`
is a type parameter of finalization rather than a flag read inside it.

**The join is a typed API in `core`, not a text format.** Both macros construct a
`Cfg` and hand it to the same `validate` → `passes` → `finalize<B>` chain, so
everything from validation down has one implementation and one test suite. The
tempting alternative — have `#[safetynet]` emit `sn_asm!` source and let rustc
expand it — was considered and rejected; §R10 records why, and §8.1's round-trip
property recovers the parity argument that alternative was reaching for.

One consequence worth naming: because the lowerer holds both the `syn` nodes and
the `Result` from `validate`, a backend rejection can be caught and reframed in
the author's vocabulary instead of surfacing as assembler jargon. That is the
main thing a textual handoff would have thrown away.

---

## 10. Keeping the compiler and layout out of the binary

The security goal: the shipped artifact contains only the finalized bytecode and
image — not the assembler, not human-readable layout metadata.

**Split finalization by what each part must know.** Everything independent of
struct layout — block layout, `BlockId`→offset backpatching, opcode encoding,
constant placement — runs **inside the proc-macro** as full std Rust and is
therefore *categorically absent* from the target (proc-macro crates and their
deps are compiler plugins, never linked). It emits a near-final program with
numeric **holes** for field offsets plus a relocation table.

Only the irreducible remainder — "fill hole *k* with the offset of path
`["body"]` in `Packet`" — runs in const-eval as a small `const fn` linker
reading `LAYOUT`. `Op::encode` lives in core alongside the interpreter, but the
target never calls it — the macros encode using their own host-side copy of core
— so it is unreferenced at runtime and falls to the same DCE argument, and the
same CI grep, as the linker below. Decoding necessarily ships; that is the VM.

Byte order costs nothing here: `LAYOUT` is order-independent by
construction (§4), so the linker patches offsets in whatever width the template
already fixed, and is generic over `B` only where it writes them — monomorphized
to the single order the build selected. Invoked only to compute `const PROGRAM`,
it is unreferenced at runtime and (expected to be) removed by DCE along with the
`LAYOUT` strings.

`inventory` is the wrong tool here: it solves distributed collection via
life-before-main registration, which would place layout data in a named linker
section in the running binary — the opposite of the goal.

**Verify, do not trust.** DCE is an optimization, not a guarantee, and this is a
security property (Review §R4). On the stripped release binary, CI must assert:
`strings | grep` finds no field names; `nm`/`cargo bloat` show no
`assemble`/`lower_block`/`backpatch` symbols. The memory image ships by
necessity, so `.rodata` holds ciphertext/hashes the guest recomputes, never
plaintext.

---

## 11. The `#[safetynet]` transformation

Applied to a function — or to a module, when intra-module calls are to be inlined
(§7.5). Expands each annotated function into:

1. **Hidden reference** — the original body renamed. Rust type-checks it (which
   is why the lowerer need not) and it is the differential oracle.
2. **Replaced public fn** — original signature, body swapped for marshal → run →
   unmarshal, all three instantiated at the function's order (`safetynet::Order`,
   or the `order = …` argument).
3. **Embedded program** — bytecode + image (see §10), built by calling `core`'s
   pipeline during expansion. `SN_DUMP_ASM=1` additionally prints the lowered
   program as `sn_asm!` source (§8.1) for inspection; it is a debugging output,
   not a build input.
4. **Bound assertions** — the `VmValue` dead-code checks.
5. **Differential harness** — `#[cfg(test)]` comparing reference and VM.

```rust
#[safetynet]
fn check(pkt: Packet) -> u32 { /* normal Rust */ }
// → __sn_ref_check (original) + check (marshal→run→unmarshal) + PROGRAM + tests
```

The lowerer can still be tested without running a VM — lower, `print`, compare
against a readable asm fixture — and the author can read what their function
became and paste that asm into an `sn_asm!` test verbatim when it looks wrong.
The difference from routing the build through that text is that here the text is
an observation of the pipeline, not a stage in it.

---

## 12. Runtime

`Vm<B: ByteOrder>`: one byte memory, one `SP`, and a dispatch loop that decodes
an `Instr` and calls its `Op::exec` — so an opcode's interpreter arm sits next to
its encoding and its `sp_delta` in the `define_ops!` table (§3.4), not in a
parallel `match`. Monomorphized to the build's byte order, so every `LD*`/`ST*`/
`LDS`/`STS` compiles to that order's byte shuffle with no dispatch on order.

Hardening every dispatch, and unconditionally for now: PC bounds; memory bounds
(in the interpreter, not emitted); `SP` bounds in both directions — overflow past
`.stack`'s end and underflow below its base are traps, the check that replaces
the old locals array's implicit in-range indexing; explicit wrapping arithmetic;
division traps (zero divisor, and `i64::MIN / -1` for the signed pair); and a
**fuel counter**, one unit per instruction, turning an infinite loop into a clean
`Trap::OutOfFuel`. `run()` returns `Result<Halt, Trap>`. Making any of this
optional is deferred: a knob is easy to add later and impossible to trust if the
unhardened path was never the tested one (§R8).

**Decode failure and trap are different types.** `DecodeErr` describes bytes that
are not a program — a truncated immediate, an unassigned opcode, an `ALLOC` whose
operand is not a multiple of 8 — and belongs to tooling: the disassembler, the
round-trip test, anything that loads an image it did not build. `Trap` describes
a program that ran and did something it may not. The interpreter decodes as it
goes, so it converts the former into `Trap::BadInstruction` at the fetch site,
but the two stay separate types: collapsing them would put "this artifact is
corrupt" and "the guest divided by zero" on one code path. A debug build additionally carries a side table of
(code offset → expected SP) emitted by finalization and asserts it at each block
entry — off in release, since it is a compiler-bug detector, not a guest-input
defense (§R9).

---

## 13. Correctness

Four oracles, each catching what the others structurally cannot:

- **Differential harness** — reference vs. VM over random and edge inputs. The
  lowering oracle, blind where §R2 and §R3 say it is blind.
- **SP validator** — the IR oracle, and since §3.1 also a correctness mechanism
  rather than a check (§R9).
- **Round-trip** — `parse → Cfg → encode → decode → print → parse` reaching a
  fixed point (§8.1): the encoding oracle, and §R8's missing property.
- **Front-end parity** — `parse(print(cfg)) == cfg` over every fixture the lowerer
  produces. Parity is a *property test, not an architecture* (§R10), and this is
  what keeps the asm surface from lagging the IR now that the lowerer does not
  travel through it.

Every suite runs under both `Le` and `Be`. Since the order is a literal and not a
build flag (§3.2), that is *test parameterization*, not a CI matrix: tests are
generic over `B` and instantiated for each order, so one `cargo test` covers
both and neither can rot unnoticed. One targeted property belongs here specifically because of
SP-relative locals: take any program, insert a balanced junk push/drop pair at a
random point, and the re-finalized program must produce identical results with
different displacements. That is the cheap test for the whole displacement
mechanism, and the one every mutation pass will need (§14).

All four grow at every phase.

---

## 14. Extension seam — mutation (deferred)

Passes are `fn(&mut Cfg, &mut Rng)` in `core`, inserted post-validation,
pre-finalization — one implementation on the shared path, so hand-written and
compiled programs are mutated by the same code and neither macro needs its own
hook. The seed comes from `#[safetynet(seed = …)]` or the `.seed` directive, and
`SN_DUMP_ASM=1` before and after a pass is how a pass gets reviewed.

The IR's `BlockId` edges and symbolic operands — including symbolic `Local(cell)`
access, whose displacements do not exist yet (§3.1) — mean passes never touch
offsets; a pass may freely add or remove stack traffic and finalization
recomputes every displacement against the new SP. What a pass *must* preserve is
`sp_in` agreement at joins; the SP validator re-runs after each and rejects the
pass, not the build, when it does not. Layout-level obfuscation (block reorder,
opcode renumbering, frame-cell/const permutation) lives in finalization; semantic
passes (opaque predicates best value) in the seam. Per-seed differential testing
is mandatory once this exists, and `sn_asm!` disassembly gives each pass a
readable before/after diff to assert on.

---

## 15. Build roadmap

Each phase ends runnable and tested; the two harnesses grow continuously.

- **Phase 0 — Workspace.** Virtual root over `crates/*`: `safetynet-core`,
  `safetynet-macros`, the `safetynet` façade, and `safetynet-demo` (the binary
  Phase 8's CI strips and greps). `ByteOrder`/`Le`/`Be` and `Word` in core, with
  both orders exercised by parameterized tests; `missing_docs` denied
  workspace-wide; release profile (`lto`, `panic="abort"`, `strip`, `opt-level`)
  pinned.
- **Phase 1 — ISA + interpreter.** One struct per opcode implementing `Op`
  (`sp_delta`, `size`, `encode`, `decode`, `exec`), the `Instr` enum, and the
  mnemonic table. The first few ops are written by hand and `define_ops!` is
  extracted once the shape has settled, rather than designed against a sketch.
  Then `Vm<B>` with byte stack, `SP`, frames (`ALLOC`/`FREE`/`LDS*`/`STS*`), all
  traps + fuel. Tested via a **throwaway hand-assembler** (superseded in Phase 3),
  which also seeds the differential harness. The suite runs under both orders from
  here on.
- **Phase 2 — IR + assembler.** `Cfg`/`Frame`/`Block`/`Terminator`, the SP
  validator folding `sp_delta()` (never a table), symbolic `Local(cell)`, block
  layout + backpatch, displacement materialization in finalization, symbolic-hole
  template form. Hand-built CFGs run on the Phase-1 interpreter. Done when a
  program with junk pushes inserted mid-block still resolves every local
  correctly.
- **Phase 3 — Macro-assembler (`sn_asm!`).** Labels → `Cfg` → validate → finalize
  → `Program<B>`, every failure a spanned error; the `print` half (disassembler)
  and the `parse → encode → decode → print → parse` fixed-point test, both orders
  (§8.1). `print`/`parse` must cover everything the IR can hold — enforced from
  Phase 6 on by running `parse(print(cfg)) == cfg` over the lowerer's output.
  Retires the Phase-1 hand-assembler and becomes the test vehicle for every phase
  after it; `.frame` cells and symbolic `load`/`store` are what make hand-written
  asm writable at all under SP-relative addressing. Done when a hand-written asm
  loop assembles, disassembles to itself, and runs to the same result under `Le`
  and `Be`. Whole-program limits (§R6) go in `core`'s validator, shared by both
  front-ends, not in the macro.
- **Phase 4 — Traits + marshalling (no macro).** `VmValue`, `VmLayout`, memory
  map, `marshal<B>`/`unmarshal<B>`, `const fn` linker, and a **hand-written
  `VmLayout`** to validate the contract before the derive exists; field-hole
  relocation exercised from `sn_asm!`.
- **Phase 5 — `#[derive(VmLayout)]` + unit enums.** Automate Phase 4; derive must
  match the hand impl byte-for-byte, in both orders.
- **Phase 6 — Lowerer + `#[safetynet]`.** ⭐ *First end-to-end.* Subset checker,
  shadow types, frame layout (cell assignment, widths, prologue/epilogue), control
  flow, short-circuit, break/continue, symbolic field paths, module-scoped inlined
  calls, the five expanded items, generated diff harness. The lowerer builds a
  `Cfg` through `core`'s API and reframes validator rejections in the author's
  vocabulary; `SN_DUMP_ASM=1` and the `parse(print(cfg)) == cfg` property are its
  own oracles. Done when the `Packet` flag-check runs on the VM and matches its
  reference on random + edge inputs.
- **Phase 7 — Subset extensions.** Data enums, `match`, `Result`/`?`, host table.
- **Phase 8 — Ship hardening.** ⭐ *Shippable.* Confirm assembler and `sn_asm!` are
  proc-macro-side, minimize/DCE the linker, CI verification on the stripped binary.
- **Phase 9 — Mutation (deferred, off critical path).**

---

# Review of this specification

An honest critique. The findings are ordered by how much they should change the
design. The first three are latent bugs the spec's own worked example either
hits or cannot rule out; the fourth is structural.

## R1 — The monomorphic `u64` machine has an unpaid masking obligation *(high)*

The spec chose one word type "to be less code," then wrote an example
(`pkt.body[i] ^ KEY[i % 5]).wrapping_add(i as u8)`) that mixes `u8`, `u32`, and
`u64` values. On a 64-bit machine, an 8-bit `wrapping_add` must wrap at 8 bits —
but `ADD` wraps at 64. Getting the reference and the VM to agree therefore
requires the lowerer to **track the logical width of every value and emit a mask
after any operation that can exceed it** (`AND 0xFF` after an 8-bit add, sign
extension for signed narrowing, etc.). The spec mentions none of this. So the
monomorphism that was supposed to remove work has instead *moved* it into a
width-tracking pass the spec doesn't have — and until that pass exists, the
flagship example is subtly wrong in release and the differential harness will
flag it (if it happens to sample a wrapping input; see R3). Either specify the
masking discipline explicitly, or admit that "monomorphic word" really means
"width-tagged word," which is most of the bookkeeping a typed-int machine would
have needed anyway.

*Partially paid by byte-granular frame cells (§3.1).* A local declared `u8`
occupies one byte, so `STS.u8` truncates for free, and truncate-at-the-end is
exact for `+ − × <<` — the residue class map from `2^64` to `2^8` is a ring
homomorphism for those. It is **not** exact for `/ % >>`, for any comparison, or
for signed narrowing, and it does nothing for a value that stays in a temporary
across those operations. So the obligation shrinks from "mask after every
narrow-width operation" to "mask before dividing, shifting right, comparing, or
sign-extending a value whose logical width is narrower than the word" — smaller,
still real, still unwritten. The lowerer still needs the width-tracking pass; the
frame just pays part of its bill.

## R2 — "Exhaustiveness is left to the reference" is unsound at the untrusted boundary *(high)*

The `match` design delegates exhaustiveness to the Rust compiler on the hidden
reference. That is valid *only for values Rust could construct*. But `.input` is
explicitly untrusted (§6.1), and `unmarshal`/field-loads read raw bytes — so a
`match` on an enum-typed *input* field can receive a discriminant that
corresponds to no variant. Rust forbids constructing an invalid enum, so the
reference cannot exhibit this case, so the differential harness **structurally
cannot catch it**, so the "the compiler proves it" argument silently doesn't
apply exactly where the bytes are adversarial. A `SWITCH` on an out-of-range tag
is then an undefined table index — a wild jump or OOB read. The spec needs a
normative rule: every lowered `match`/`SWITCH` on a value that can originate from
`.input` must have a synthesized default that traps, and marshalling of enums
must validate tags. This is a soundness gap, not a nicety.

## R3 — The differential harness is weakest precisely where the challenge lives *(high)*

The spec leans on random differential testing as *the* lowering oracle. But the
entire point of a flag checker is one accepting input among ~2^k. Random sampling
will essentially never hit the accept path, so the harness validates the
*rejecting* behavior thoroughly and the *accepting* behavior almost never — and
the accept path is the one with the interesting arithmetic (R1's wrapping, the
final comparison, the epilogue). "Includes edge cases" does not fix this;
measure-zero is measure-zero. The harness must be supplemented with **targeted
vectors**: the known-good flag, single-bit and single-byte near-misses around it,
and property tests that force each `match` arm and each `?`-`Err` path to be
taken. Without those, a lowering bug on the accept path ships green.

## R4 — The security goal rests on an optimization, and the mechanism that resolves offsets is the same one that endangers it *(high, structural)*

§10's "assembler absent from the binary" splits into two very different
guarantees. The proc-macro-side work is absent *categorically* — solid. But the
residual `const fn` linker is absent only *by DCE*, which is an optimization, not
a contract, and the spec is asking an optimization to enforce a security
property. Worse, this residual exists **only because** offset resolution was
deferred to a type-directed phase (§6.2) — the very thing that made struct inputs
work is what drags a piece of the compiler toward the binary. The tension is real
and only half-resolved. Two honest options the spec should choose between: (a)
prove the linker is small enough to be genuinely `const`-evaluable on stable
(no heap — nested-path relocation over variable-depth layouts in `const fn` is
not obviously feasible and the spec never demonstrates it), accepting the
verify-don't-trust CI gate as the real guarantee; or (b) move the whole pipeline
into `build.rs` codegen so the assembler is categorically absent, accepting that
this abandons the proc-macro-as-lowerer model (build.rs cannot see macro output).
The spec currently gestures at (a) without confronting that the `const fn`
constraint may be infeasible, in which case it silently falls back to `LazyLock`
— which ships the whole assembler. This is the least-proven load-bearing claim in
the document.

## R5 — Reference and VM disagree on overflow, profile-dependently *(medium)*

The VM always wraps. The reference is compiled normally, where `+` on `u32`
panics on overflow in debug and wraps in release. So the reference's semantics
depend on the build profile while the VM's are fixed, and a plain `+` in guest
code makes the differential result profile-dependent. Nothing forces the author
to write `wrapping_*`. The subset checker should either reject bare arithmetic
operators in favor of explicit `wrapping_`/`checked_` methods, or the spec should
declare the VM matches one profile normatively and require the harness to run
under it. Related to R1; both are symptoms of the arithmetic semantics never
being pinned down.

## R6 — "Complexity-limited" is load-bearing and undefined *(medium)*

The phrase appears repeatedly and justifies major decisions (inline-all calls, no
spilling, constant stack depth), yet no actual limit is specified. With calls
inlined and no code-size bound, a function called from many sites inside loops
produces superlinear bytecode with no diagnostic. The spec bounds *runtime* (fuel)
but never *compile-time code size*. Define the subset's limits concretely — max
locals, max inline expansion, max block count — and emit a spanned error when
exceeded, or "complexity-limited Rust" remains an undefined input language.

## R7 — Phase 8 ships a textbook stack VM; the difficulty is deferred past the "shippable" milestone *(medium)*

The roadmap stars Phase 8 as "shippable challenge," but mutation — the only thing
that makes the artifact more than a recognizable vanilla stack VM with a labeled
`HOST` boundary and a readable image — is Phase 9, *after* it. By the spec's own
earlier admission that a stack machine is the most recognizable design there is,
the Phase-8 artifact is mechanically shippable but likely lifts in an hour. Two
things now cut slightly against that: a big-endian build (§3.2), and SP-relative
frames in a single address space (§3.1), which read as a native-ish ABI rather
than the textbook `LOADL 3` that a decompiler-shaped brain pattern-matches in
seconds. Both are speed bumps, neither is a substitute for mutation. The
milestone label oversells. Either fold a minimum of opaque-predicate + opcode-
renumbering mutation into the "shippable" bar, or rename the milestone to
"functionally complete, not yet hard."

## R9 — SP-relative locals move the depth invariant onto the correctness path *(medium, new)*

Removing the locals array in favor of `SP`-relative byte displacements (§3.1) is
a good trade for the reversing goal and for aggregates, but it changes what a
tracking bug *does*. With absolute indices, miscounting the operand stack
produced a stack runaway or a validator scream: loud, local, unmissable. With
displacements, the same miscount produces `LDS.u32 40` where `LDS.u32 48` was
meant — a clean read of the wrong cell, which is a plausible value, so the program
keeps running and the differential harness catches it only if a sampled input
happens to distinguish the two cells (and see §R3 for how weak that sampling is
on the interesting path). The invariant is no longer a nicety that catches
lowering mistakes; it is the thing that makes local addressing mean anything.

Three mitigations, and the spec should commit to all three: `sp_delta` on the op
rather than in a central table (§3.4), so the model cannot go stale as the ISA
grows; `sp_in` recorded by the front-end and *checked* by the validator rather
than computed by it, so the two disagree loudly instead of agreeing wrongly; and
the debug-build (offset → expected SP) side table (§12), which turns a
displacement bug into an assertion at the nearest block boundary instead of a
wrong answer three thousand instructions later. The junk-push property test
(§13) is the cheap external check. None of this is expensive — but skipping it
trades a compile-time error class for a silent-wrong-answer class, which is the
wrong direction for a project whose whole validation story is a differential
harness.

---

## R10 — The lowerer emits a `Cfg`, not asm text; the textual handoff was considered and rejected *(recorded decision)*

The tempting move is to have `#[safetynet]` lower the Rust subset to `sn_asm!`
tokens and let rustc expand that — one path to bytecode, parity by construction.
It was specified that way briefly and then reverted, because the argument does
not survive inspection:

- **It doesn't remove a second implementation, because there wasn't one.** Both
  macros already call the same `validate`/`passes`/`finalize` in `core` — §2's
  entire reason for existing. The handoff replaces a *typed* interface (`Cfg`)
  with an untyped one (a text grammar) and inserts a printer and parser between
  two components in the same process. That is pretty-print-and-reparse between
  compiler passes; note that clang does not emit `.ll` and re-read it, it calls
  the API, and LLVM's textual IR exists for tooling and tests. Which is exactly
  the arrangement kept here.
- **Diagnostics get worse and can invert.** With the API the lowerer holds the
  `syn` nodes when `validate` returns `Err` and can reframe the failure in Rust
  terms. With a handoff the lowerer has already exited: errors are raised by a
  different expansion, in assembler vocabulary, and a *lowerer* bug that emits
  malformed asm surfaces as a parse error pointed — via forwarded spans — at
  innocent user code. The compiler blames the author for its own bug.
- **Every compiler internal becomes public syntax.** Width tags for §R1's masking
  pass, provenance, inline markers, seeds: each would need asm syntax, until the
  hand-writable assembly language is a serialization format wearing a mnemonic
  costume — destroying the property that motivated it.
- **The simplification is small.** The lowerer still does frame layout, still
  builds a block graph, still resolves break/continue targets; only its last step
  changes from `return cfg` to `print(cfg)`. What is actually removed is a
  dependency on core's IR types, which costs nothing.
- **Costs that don't go away.** A second full macro expansion and a TokenStream
  round-trip per function on every rebuild; and no eager expansion, so asm
  fragments cannot be composed across invocations without continuation-passing
  `macro_rules!` — a permanent tax on anything the design later wants to compose.
- **Snapshot tests are weaker than they look.** A golden asm file asserts the
  lowerer produced *the text last accepted*, not correct text, and every codegen
  improvement churns every snapshot until reviewers stop reading the diffs.

What the handoff was genuinely reaching for is **dogfooding**: under the API
design the asm surface can quietly lag the IR until someone tries to hand-write
something. That is recovered without the coupling by the §8.1 property —
`parse(print(cfg)) == cfg` over every lowerer fixture — which turns a surface gap
into a failing test, and by `SN_DUMP_ASM=1`, which supplies the `cargo expand`
debugging story. Parity by property, not by putting a parser on the production
path.

The residual risk to watch: a property test can be quietly narrowed (fixtures that
happen not to exercise a construct), whereas the handoff could not be. If the
fixture set is not kept honest, the surface rots anyway — just silently instead of
loudly.

---

## R8 — Smaller issues

- **Endianness** *(addressed)* is now a compile-time type parameter (§3) threaded
  through `Vm<B>`, the image builder and `marshal<B>`/`unmarshal<B>`, so agreement
  is enforced by the type checker rather than by a code comment. What the type
  system cannot enforce is coverage: the differential, golden and round-trip
  suites must each be instantiated for both `Le` and `Be` (§13), or the order
  nobody builds becomes the order nobody has ever tested.
- **No encode/decode round-trip test** *(addressed)*. The throwaway hand-assembler
  is deleted after Phase 1, which originally left no property that
  `decode(encode(x)) == x`; a symmetric encoder/decoder bug would pass the
  behavioral diff yet corrupt any future tooling. Phase 3's `sn_asm!` plus its
  disassembler replace it permanently, and the fixed-point round-trip is that
  phase's exit criterion.
- **Three overlapping representations for array-like data** — a `[u8; N]` const
  in `.rodata`, a `Slice { ptr, len }`, and an `Aggregate` — are never unified.
  The example's `KEY`/`CT` indexing works by implication; specify which
  representation a const array takes and how indexing lowers for each.
- **"Cannot disagree" assumes a coherent build.** Separate compilation with a
  stale artifact could desync a struct's layout between its derive site and a
  `#[safetynet]` use site. Cargo normally prevents this; state the assumption.
- **The single-error-type `?` restriction** collides with per-host-call error
  types (§7.4/§7.5): fallible host calls cannot each carry their own error and
  still be used with `?`. Minor, but name it.
- **Unconditional interpreter hardening vs. size/recognizability** *(deferred by
  decision)*. Bounds checks and fuel target "players feed garbage," which only
  bites if players supply input. For a pure baked-input reversing challenge, some
  hardening is dead weight that also enlarges and clarifies the dispatch loop.
  Gating it on the threat model remains the right end state, but everything is
  unconditional to begin with: the stripped-down path has to be a deviation from
  something tested, not the only thing ever built.

## What holds up

The IR-centric structure is right: one CFG plus a flat encoding, with mutation as
a deferred seam, is the correct amount of machinery and correctly resists the
MIR/LIR over-engineering. The single-source-of-truth layout via a trait const is
the right idea *given* the proc-macro architecture (R4 is about its cost, not its
correctness). The hidden-reference-as-oracle is a strong pattern, bounded by R2/
R3/R5 which are about what it *can't* see. And the SP invariant as a build-time
oracle is the best decision in the document — it converts the stack machine's one
new bug class into a spanned compile error, though §R9 notes that SP-relative
locals raise the stakes on getting it right, and §3.4's opcode-as-type table is
what keeps it honest as the ISA grows. The bones are sound; the gaps are in
arithmetic semantics (R1/R5), the trust boundary (R2), test coverage of the accept
path (R3), and one over-claimed security guarantee (R4).
