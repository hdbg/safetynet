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

Struct fields carry one extra rule: a field's first use names its type with
[`Typed::typed`], and later uses may go bare. Naming two different types for
one field is a compile error.

```rust
use safetynet::Typed;

#[safetynet]
fn check(pkt: Packet) -> u32 {
    if pkt.seq.typed::<u32>() > 100 {   // first use names the type
        return 100;
    }
    pkt.seq                             // known from here on
}
```

The macro sees only the annotated function's tokens — not the struct
definition — so it cannot resolve a field's type on its own; `.typed::<u32>()`
supplies it. The call is an identity function bounded so it compiles only when
the field is *exactly* the named type: name the wrong one and rustc rejects
the hidden reference copy of your function, pointing at the call. A field used
before its type is named is refused with an error that spells out the fix.

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
