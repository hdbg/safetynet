![safetynet](docs/banner.png)


A Rust-embedded bytecode VM for reversing challenges: guest code becomes
packed bytecode over a stack machine, built entirely at compile time.

> **Status: early development.** The design is fully specified in
> [`docs/spec.md`](docs/spec.md).

## How it works

The eventual surface is a normal Rust function:

```rust
#[safetynet]
fn check(pkt: Packet) -> u32 {
    // ordinary Rust — the VM is invisible here
}
```

Expanding `#[safetynet]` produces the **replaced function** (body swapped for
*marshal → run → unmarshal*), the **embedded program** (bytecode plus memory
image, built at compile time).

`cargo expand` shows there is no IR or interpreter left in the output — just
finished bytecode, a compile-time linker call that bakes field constants into
the bytes, and a relocation for each region-base address the program cannot
settle on its own. `finalize` runs the relocations against a layout and returns
runnable bytecode; the VM executes it in the program's byte order:

```rust
let program = artifact.finalize(&layout)?;   // fills each placeholder push with its region's base
let vm = Vm::<Le>::new(image).run(&program, fuel)?;   // result left on the stack
```

`Vm<Be>` will not run a `Program<Le>` — the byte order is part of the type.

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
                     layout descriptors, interpreter
  safetynet-macros   proc-macros: #[safetynet], #[derive(VmLayout)]
  safetynet          the façade crate downstream code depends on
  safetynet-demo     a sample cipher lowered from Rust with #[safetynet]
docs/spec.md         the full specification and design review
```
