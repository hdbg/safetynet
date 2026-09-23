//! The methods the subset knows, and what they are called on.
//!
//! The lowerer cannot see a receiver's Rust type, only its shape: a place in
//! the aggregate parameter, a constant table of the body, or a walk one of
//! these methods started over either. A method is looked up by that shape and
//! its name, so adding one is a table entry and a handler, and the expression
//! lowering names no method at all.

use quote::ToTokens;

use super::build::{Lowerer, err, internal};
use super::ty::Scalar;

/// A field path into the aggregate parameter, or the parameter itself when the
/// path is empty. What it holds is unknown until a method or `.typed()` says.
#[derive(Clone)]
pub(super) struct Place {
    pub(super) ty: syn::Type,
    pub(super) path: Vec<syn::Ident>,
}

/// A constant table of the body, placed in `.rodata`: where it starts, how
/// many elements it holds, and what each one is. All three are known at
/// expansion, so nothing about it is read from memory but the elements.
#[derive(Clone, Copy)]
pub(super) struct Table {
    pub(super) off: u32,
    pub(super) len: u32,
    pub(super) elem: Scalar,
}

/// What a walk runs over.
///
/// A place carries the aggregate's type, a table three numbers; the value
/// lives only for the one chain it classifies, so the gap costs nothing.
#[expect(clippy::large_enum_variant)]
#[derive(Clone)]
pub(super) enum Source {
    Place(Place),
    Table(Table),
}

/// What a method is called on.
pub(super) enum Receiver {
    Place(Place),
    Table(Table),
    /// A walk over a source's elements, as a `for` consumes it. Whether Rust
    /// yields a `&T` or a `T` does not reach the machine: the cell holds the
    /// element either way.
    Walk(Source),
}

/// The shape a table entry is keyed by.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Kind {
    Place,
    Table,
    Walk,
}

impl Kind {
    /// How a refusal names the shape.
    pub(super) const fn describe(self) -> &'static str {
        match self {
            Self::Place => "a field of the aggregate parameter",
            Self::Table => "a constant table",
            Self::Walk => "a walk over a byte region",
        }
    }
}

impl Receiver {
    pub(super) const fn kind(&self) -> Kind {
        match self {
            Self::Place(_) => Kind::Place,
            Self::Table(_) => Kind::Table,
            Self::Walk(_) => Kind::Walk,
        }
    }

    /// The place a value method was called on; the table only pairs these
    /// methods with places, so anything else is a table error.
    fn place(self, node: impl ToTokens) -> syn::Result<Place> {
        match self {
            Self::Place(place) => Ok(place),
            Self::Table(_) | Self::Walk(_) => {
                Err(internal(node, "a place method reached elsewhere"))
            }
        }
    }

    /// The constant table a value method was called on.
    fn table(self, node: impl ToTokens) -> syn::Result<Table> {
        match self {
            Self::Table(table) => Ok(table),
            Self::Place(_) | Self::Walk(_) => {
                Err(internal(node, "a table method reached elsewhere"))
            }
        }
    }
}

/// One method the machine lowers.
pub(super) struct Method {
    pub(super) on: Kind,
    pub(super) name: &'static str,
    /// Whether the call names a type, as `.typed::<u32>()` does. Every other
    /// method refuses one.
    pub(super) turbofish: bool,
    pub(super) handler: Handler,
}

/// What a method does when lowered.
pub(super) enum Handler {
    /// Leaves a word on the stack. `result` is its type when the name alone
    /// fixes it; `None` when the turbofish names it.
    Value {
        result: Option<Scalar>,
        lower: Lower,
    },
    /// Starts a walk over the receiver's elements, for a `for` to consume.
    Walk,
    /// Hands the receiver back as it is: `.as_bytes()` on a place or a table
    /// and `.copied()` on a walk change what Rust sees, not what the machine
    /// reads.
    Same,
}

/// A value method's lowering: the receiver, the call, and the operand depth.
type Lower = fn(&mut Lowerer, Receiver, &syn::ExprMethodCall, u32) -> syn::Result<Scalar>;

/// Every method the subset lowers.
pub(super) static METHODS: &[Method] = &[
    Method {
        on: Kind::Place,
        name: "typed",
        turbofish: true,
        handler: Handler::Value {
            result: None,
            lower: typed,
        },
    },
    Method {
        on: Kind::Place,
        name: "len",
        turbofish: false,
        handler: Handler::Value {
            result: Some(Scalar::U64),
            lower: len,
        },
    },
    Method {
        on: Kind::Place,
        name: "iter",
        turbofish: false,
        handler: Handler::Walk,
    },
    Method {
        on: Kind::Place,
        name: "bytes",
        turbofish: false,
        handler: Handler::Walk,
    },
    Method {
        on: Kind::Place,
        name: "as_bytes",
        turbofish: false,
        handler: Handler::Same,
    },
    Method {
        on: Kind::Walk,
        name: "copied",
        turbofish: false,
        handler: Handler::Same,
    },
    Method {
        on: Kind::Table,
        name: "len",
        turbofish: false,
        handler: Handler::Value {
            result: Some(Scalar::U64),
            lower: table_len,
        },
    },
    Method {
        on: Kind::Table,
        name: "iter",
        turbofish: false,
        handler: Handler::Walk,
    },
    Method {
        on: Kind::Table,
        name: "bytes",
        turbofish: false,
        handler: Handler::Walk,
    },
    Method {
        on: Kind::Table,
        name: "as_bytes",
        turbofish: false,
        handler: Handler::Same,
    },
];

/// The method `name` on a receiver of shape `on`, if the machine lowers one.
pub(super) fn find(on: Kind, name: &str) -> Option<&'static Method> {
    METHODS
        .iter()
        .find(|method| method.on == on && method.name == name)
}

/// The `T` of `.typed::<T>()`, when the turbofish names exactly one type.
pub(super) fn typed_argument(call: &syn::ExprMethodCall) -> Option<&syn::Type> {
    let turbofish = call.turbofish.as_ref()?;
    match turbofish.args.first() {
        Some(syn::GenericArgument::Type(ty)) if turbofish.args.len() == 1 => Some(ty),
        _ => None,
    }
}

/// `p.x.typed::<u64>()`: a field read whose first use names the field's type.
///
/// The field's own type is not visible here, so its first use names one; later
/// uses may go bare. The reference copy calls the real `Typed::typed`, which
/// compiles only when the named type is exactly the field's own.
fn typed(
    lowerer: &mut Lowerer,
    receiver: Receiver,
    call: &syn::ExprMethodCall,
    _: u32,
) -> syn::Result<Scalar> {
    let place = receiver.place(call)?;
    if place.path.is_empty() {
        return Err(err(call, "only a field names its type with `.typed()`"));
    }
    let ty = typed_argument(call)
        .ok_or_else(|| err(call, "`.typed()` needs the type: `.typed::<u32>()`"))?;
    let scalar = Scalar::of(ty)
        .ok_or_else(|| err(ty, "a field must be typed as a scalar the machine can hold"))?;
    lowerer.name_field_type(&place, scalar, ty.to_token_stream().to_string(), call)?;
    lowerer.read_place(&place, scalar, call)
}

/// `region.len()`: the header's length, checked against the input.
///
/// The reference copy sees a `usize`, which the machine holds as a word; guest
/// code narrows it with `as`, as Rust would have it.
fn len(
    lowerer: &mut Lowerer,
    receiver: Receiver,
    call: &syn::ExprMethodCall,
    depth: u32,
) -> syn::Result<Scalar> {
    let place = receiver.place(call)?;
    let region = lowerer.load_region(&place, depth, call)?;
    lowerer.load(region.len)?;
    Ok(Scalar::U64)
}

/// `TABLE.len()`: known when the table was declared, so it is an immediate.
fn table_len(
    lowerer: &mut Lowerer,
    receiver: Receiver,
    call: &syn::ExprMethodCall,
    _: u32,
) -> syn::Result<Scalar> {
    let table = receiver.table(call)?;
    lowerer.push_word(u64::from(table.len))?;
    Ok(Scalar::U64)
}
