![safetynet](docs/banner.png)

# safetynet

A Rust-embedded bytecode VM. You annotate a normal
Rust function; `safetynet` lowers it to an intermediate representation, compiles
that to bytecode, and replaces the function body with a call into a small stack
machine.


> **Status: early development.** The design is fully specified in
> [`docs/spec.md`](docs/spec.md)

## How it works

```rust
#[safetynet]
fn check(pkt: Packet) -> u32 {
    // ordinary Rust — the VM is invisible here
}
```

Expanding `#[safetynet]` produces, from that one function:

- the **replaced public function** — same signature, body swapped for
  *marshal → run → unmarshal*;
- the **embedded program** — bytecode plus a memory image, built at compile time;
- **differential tests** comparing the reference against the VM over random and
  edge inputs.

The assembler, layout metadata, and the compiler itself run proc-macro-side, so
they are categorically absent from the target binary — only the finalized
bytecode, the memory image, and the interpreter ship.

## Design highlights

- **Stack machine, one address space.** A single flat byte memory and one stack
  pointer; locals, temporaries, and frame-resident aggregates are all just bytes
  below `SP`, addressed by compile-time-computed displacement.
- **Byte order is a type parameter.** The machine is `Vm<B: ByteOrder>` with
  `Le`/`Be` implementors, threaded from macro expansion through the interpreter
  and both marshal directions — so guest access and host marshalling cannot
  disagree; a mismatch is a type error, not a wrong answer.
- **Deferred offset resolution.** Guest field accesses stay symbolic through the
  IR and are resolved from a `VmLayout` derive during finalization, so the encoder
  and decoder cannot drift.
- **`sn_asm!` front-end.** A textual assembler over the same IR, kept honest by a
  `parse(print(cfg)) == cfg` round-trip property, giving the compiler a readable
  disassembly surface and a home for hand-written guest routines.
- **Correctness by construction.** A differential harness, an SP validator, an
  encoding round-trip, and front-end parity — each run under both byte orders via
  test parameterization rather than a CI matrix.

## Supported Rust subset

Guest functions are complexity-limited: `if`/`while`/`loop`/`for a..b`,
`break`/`continue`, short-circuiting `&&`/`||`, `match`, field-less and
data-carrying enums, `Result` and `?` (single error type), and module-scoped
inlined calls. No recursion, heap, generics, traits, closures, or floats in guest
code. Out-of-subset constructs are rejected at compile time with a pointing error.

## Workspace

```
crates/
  safetynet-core     opcode table, IR types, traits, byte-order policy,
                     layout descriptors, assembler, interpreter
  safetynet-macro    proc-macros: #[safetynet], #[derive(VmLayout)], sn_asm!
src/                 façade / demo binary
docs/spec.md         the full specification and design review
```

## Building

Toolchain versions are pinned via [mise](https://mise.jdx.dev/) (Rust, edition
2024).

```sh
cargo build --workspace
cargo test --workspace
```

CI checks formatting, clippy (warnings denied), tests and doc tests under both
byte orders, and the release build.

## Documentation

[`docs/spec.md`](docs/spec.md) is the single source of truth: machine model and
instruction set, the type and memory models, the compilation pipeline, the plan
for keeping the compiler out of the binary, a phased build roadmap, and a candid
review of the design's own open problems.
