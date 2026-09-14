![safetynet](docs/banner.png)


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

**Not supported**

- Recursion
- Heap allocation
- Generics, traits, closures
- Floats
