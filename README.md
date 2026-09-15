![safetynet](docs/banner.png)

[![AI Slop Inside](https://sladge.net/badge.svg)](https://sladge.net)


A Rust-embedded bytecode VM for reversing challenges: guest code becomes
packed bytecode over a stack machine, built entirely at compile time.

> **Status: Proof of Concept.** Current work focuses on hardening and adding variety to the VM

## Why
`safetynet` is a Rust library for building reverse-engineering puzzles. 

You write an ordinary function, tag it with `#[safetynet]`, and at compile time the macro rewrites its body into bytecode for a tiny made-up CPU plus an interpreter that runs it -- so the shipped binary no longer contains recognizable machine code for your logic, just a little VM and an opaque blob someone has to decode. 

The function still behaves identically to callers, and the build validates the VM against your original Rust so the hidden version is provably equivalent to what you wrote. It's essentially a small compiler that runs entirely inside rustc.

## How it works

The eventual surface is a normal Rust function:

```rust
#[safetynet]
fn check(pkt: Packet) -> u32 {
    // ordinary Rust — the VM is invisible here
}
```

Expanding `#[safetynet]` produces the **replaced function** and the **embedded program** (bytecode plus memory
image, built at compile time).


## Supported Rust subset

Guest functions are complexity-limited. Out-of-subset constructs are rejected at
compile time with an error pointing at the offending code.

**Supported**

- Control flow: `if`, `while`, `loop`, `for a..b`, `break`, `continue`
- Short-circuiting `&&` and `||`
- `match`
- Enums, both field-less and data-carrying (WIP)
- `Result` and `?`, with a single error type (WIP)
- Function calls (WIP)

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
**Not supported**

- Recursion
- Heap allocation
- Generics, traits, closures
- Floats
