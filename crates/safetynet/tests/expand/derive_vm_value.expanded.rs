#![allow(dead_code)]
use safetynet::VmValue;
enum Kind {
    A,
    B,
    C,
}
impl ::safetynet::__private::Sealed for Kind {}
#[automatically_derived]
impl ::safetynet::VmValue for Kind {
    fn to_word(self) -> ::safetynet::Word {
        self as ::safetynet::Word
    }
    fn from_word(__word: ::safetynet::Word) -> Self {
        match __word {
            __w if __w == Kind::A as ::safetynet::Word => Kind::A,
            __w if __w == Kind::B as ::safetynet::Word => Kind::B,
            __w if __w == Kind::C as ::safetynet::Word => Kind::C,
            _ => Kind::A,
        }
    }
}
#[automatically_derived]
#[doc(hidden)]
unsafe impl ::core::clone::TrivialClone for Kind {}
#[automatically_derived]
impl ::core::clone::Clone for Kind {
    #[inline]
    fn clone(&self) -> Kind {
        *self
    }
}
#[automatically_derived]
impl ::core::marker::Copy for Kind {}
#[repr(u8)]
enum Tag {
    Lo = 3,
    Hi = 200,
}
impl ::safetynet::__private::Sealed for Tag {}
#[automatically_derived]
impl ::safetynet::VmValue for Tag {
    fn to_word(self) -> ::safetynet::Word {
        self as ::safetynet::Word
    }
    fn from_word(__word: ::safetynet::Word) -> Self {
        match __word {
            __w if __w == Tag::Lo as ::safetynet::Word => Tag::Lo,
            __w if __w == Tag::Hi as ::safetynet::Word => Tag::Hi,
            _ => Tag::Lo,
        }
    }
}
#[automatically_derived]
#[doc(hidden)]
unsafe impl ::core::clone::TrivialClone for Tag {}
#[automatically_derived]
impl ::core::clone::Clone for Tag {
    #[inline]
    fn clone(&self) -> Tag {
        *self
    }
}
#[automatically_derived]
impl ::core::marker::Copy for Tag {}
fn main() {}
