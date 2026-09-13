#![allow(dead_code)]
use safetynet::VmLayout;
struct Header {
    seq: u32,
    flags: u8,
}
#[doc(hidden)]
#[allow(non_upper_case_globals)]
impl Header {
    const __SN_VMLAYOUT: (&'static [::safetynet::Field], usize, usize) = {
        const __sn_a0: usize = <u32 as ::safetynet::VmLayout>::ALIGN;
        const __sn_s0: usize = <u32 as ::safetynet::VmLayout>::SIZE;
        const __sn_o0: usize = (0usize).next_multiple_of(__sn_a0);
        const __sn_a1: usize = <u8 as ::safetynet::VmLayout>::ALIGN;
        const __sn_s1: usize = <u8 as ::safetynet::VmLayout>::SIZE;
        const __sn_o1: usize = (__sn_o0 + __sn_s0).next_multiple_of(__sn_a1);
        const __sn_align: usize = {
            let mut __m = 1usize;
            if __sn_a0 > __m {
                __m = __sn_a0;
            }
            if __sn_a1 > __m {
                __m = __sn_a1;
            }
            __m
        };
        const __sn_size: usize = (__sn_o1 + __sn_s1).next_multiple_of(__sn_align);
        (
            &[
                ::safetynet::Field::new(
                    "seq",
                    __sn_o0 as u32,
                    __sn_s0 as u32,
                    if <u32 as ::safetynet::VmLayout>::LAYOUT.fields().is_empty() {
                        ::core::option::Option::None
                    } else {
                        ::core::option::Option::Some(
                            <u32 as ::safetynet::VmLayout>::LAYOUT,
                        )
                    },
                ),
                ::safetynet::Field::new(
                    "flags",
                    __sn_o1 as u32,
                    __sn_s1 as u32,
                    if <u8 as ::safetynet::VmLayout>::LAYOUT.fields().is_empty() {
                        ::core::option::Option::None
                    } else {
                        ::core::option::Option::Some(
                            <u8 as ::safetynet::VmLayout>::LAYOUT,
                        )
                    },
                ),
            ],
            __sn_size,
            __sn_align,
        )
    };
}
#[automatically_derived]
impl ::safetynet::VmLayout for Header {
    const LAYOUT: &'static ::safetynet::TypeLayout = {
        const __L: ::safetynet::TypeLayout = ::safetynet::TypeLayout::new(
            Header::__SN_VMLAYOUT.0,
        );
        &__L
    };
    const SIZE: usize = Header::__SN_VMLAYOUT.1;
    const ALIGN: usize = Header::__SN_VMLAYOUT.2;
    fn marshal<B: ::safetynet::ByteOrder>(&self, __mem: &mut [u8]) {
        let __f = <Self as ::safetynet::VmLayout>::LAYOUT.fields();
        if let ::core::option::Option::Some(__fd) = __f.get(0) {
            let __o = __fd.offset() as usize;
            if let ::core::option::Option::Some(__slot) = __mem
                .get_mut(__o..__o + __fd.size() as usize)
            {
                ::safetynet::VmLayout::marshal::<B>(&self.seq, __slot);
            }
        }
        if let ::core::option::Option::Some(__fd) = __f.get(1) {
            let __o = __fd.offset() as usize;
            if let ::core::option::Option::Some(__slot) = __mem
                .get_mut(__o..__o + __fd.size() as usize)
            {
                ::safetynet::VmLayout::marshal::<B>(&self.flags, __slot);
            }
        }
    }
    fn unmarshal<B: ::safetynet::ByteOrder>(__mem: &[u8]) -> Self {
        let __f = <Self as ::safetynet::VmLayout>::LAYOUT.fields();
        Self {
            seq: {
                let __sub: &[u8] = match __f.get(0) {
                    ::core::option::Option::Some(__fd) => {
                        let __o = __fd.offset() as usize;
                        __mem.get(__o..__o + __fd.size() as usize).unwrap_or(&[])
                    }
                    ::core::option::Option::None => &[],
                };
                <u32 as ::safetynet::VmLayout>::unmarshal::<B>(__sub)
            },
            flags: {
                let __sub: &[u8] = match __f.get(1) {
                    ::core::option::Option::Some(__fd) => {
                        let __o = __fd.offset() as usize;
                        __mem.get(__o..__o + __fd.size() as usize).unwrap_or(&[])
                    }
                    ::core::option::Option::None => &[],
                };
                <u8 as ::safetynet::VmLayout>::unmarshal::<B>(__sub)
            },
        }
    }
}
struct Packet {
    kind: u8,
    header: Header,
    len: u16,
    tag: u64,
}
#[doc(hidden)]
#[allow(non_upper_case_globals)]
impl Packet {
    const __SN_VMLAYOUT: (&'static [::safetynet::Field], usize, usize) = {
        const __sn_a0: usize = <u8 as ::safetynet::VmLayout>::ALIGN;
        const __sn_s0: usize = <u8 as ::safetynet::VmLayout>::SIZE;
        const __sn_o0: usize = (0usize).next_multiple_of(__sn_a0);
        const __sn_a1: usize = <Header as ::safetynet::VmLayout>::ALIGN;
        const __sn_s1: usize = <Header as ::safetynet::VmLayout>::SIZE;
        const __sn_o1: usize = (__sn_o0 + __sn_s0).next_multiple_of(__sn_a1);
        const __sn_a2: usize = <u16 as ::safetynet::VmLayout>::ALIGN;
        const __sn_s2: usize = <u16 as ::safetynet::VmLayout>::SIZE;
        const __sn_o2: usize = (__sn_o1 + __sn_s1).next_multiple_of(__sn_a2);
        const __sn_a3: usize = <u64 as ::safetynet::VmLayout>::ALIGN;
        const __sn_s3: usize = <u64 as ::safetynet::VmLayout>::SIZE;
        const __sn_o3: usize = (__sn_o2 + __sn_s2).next_multiple_of(__sn_a3);
        const __sn_align: usize = {
            let mut __m = 1usize;
            if __sn_a0 > __m {
                __m = __sn_a0;
            }
            if __sn_a1 > __m {
                __m = __sn_a1;
            }
            if __sn_a2 > __m {
                __m = __sn_a2;
            }
            if __sn_a3 > __m {
                __m = __sn_a3;
            }
            __m
        };
        const __sn_size: usize = (__sn_o3 + __sn_s3).next_multiple_of(__sn_align);
        (
            &[
                ::safetynet::Field::new(
                    "kind",
                    __sn_o0 as u32,
                    __sn_s0 as u32,
                    if <u8 as ::safetynet::VmLayout>::LAYOUT.fields().is_empty() {
                        ::core::option::Option::None
                    } else {
                        ::core::option::Option::Some(
                            <u8 as ::safetynet::VmLayout>::LAYOUT,
                        )
                    },
                ),
                ::safetynet::Field::new(
                    "header",
                    __sn_o1 as u32,
                    __sn_s1 as u32,
                    if <Header as ::safetynet::VmLayout>::LAYOUT.fields().is_empty() {
                        ::core::option::Option::None
                    } else {
                        ::core::option::Option::Some(
                            <Header as ::safetynet::VmLayout>::LAYOUT,
                        )
                    },
                ),
                ::safetynet::Field::new(
                    "len",
                    __sn_o2 as u32,
                    __sn_s2 as u32,
                    if <u16 as ::safetynet::VmLayout>::LAYOUT.fields().is_empty() {
                        ::core::option::Option::None
                    } else {
                        ::core::option::Option::Some(
                            <u16 as ::safetynet::VmLayout>::LAYOUT,
                        )
                    },
                ),
                ::safetynet::Field::new(
                    "tag",
                    __sn_o3 as u32,
                    __sn_s3 as u32,
                    if <u64 as ::safetynet::VmLayout>::LAYOUT.fields().is_empty() {
                        ::core::option::Option::None
                    } else {
                        ::core::option::Option::Some(
                            <u64 as ::safetynet::VmLayout>::LAYOUT,
                        )
                    },
                ),
            ],
            __sn_size,
            __sn_align,
        )
    };
}
#[automatically_derived]
impl ::safetynet::VmLayout for Packet {
    const LAYOUT: &'static ::safetynet::TypeLayout = {
        const __L: ::safetynet::TypeLayout = ::safetynet::TypeLayout::new(
            Packet::__SN_VMLAYOUT.0,
        );
        &__L
    };
    const SIZE: usize = Packet::__SN_VMLAYOUT.1;
    const ALIGN: usize = Packet::__SN_VMLAYOUT.2;
    fn marshal<B: ::safetynet::ByteOrder>(&self, __mem: &mut [u8]) {
        let __f = <Self as ::safetynet::VmLayout>::LAYOUT.fields();
        if let ::core::option::Option::Some(__fd) = __f.get(0) {
            let __o = __fd.offset() as usize;
            if let ::core::option::Option::Some(__slot) = __mem
                .get_mut(__o..__o + __fd.size() as usize)
            {
                ::safetynet::VmLayout::marshal::<B>(&self.kind, __slot);
            }
        }
        if let ::core::option::Option::Some(__fd) = __f.get(1) {
            let __o = __fd.offset() as usize;
            if let ::core::option::Option::Some(__slot) = __mem
                .get_mut(__o..__o + __fd.size() as usize)
            {
                ::safetynet::VmLayout::marshal::<B>(&self.header, __slot);
            }
        }
        if let ::core::option::Option::Some(__fd) = __f.get(2) {
            let __o = __fd.offset() as usize;
            if let ::core::option::Option::Some(__slot) = __mem
                .get_mut(__o..__o + __fd.size() as usize)
            {
                ::safetynet::VmLayout::marshal::<B>(&self.len, __slot);
            }
        }
        if let ::core::option::Option::Some(__fd) = __f.get(3) {
            let __o = __fd.offset() as usize;
            if let ::core::option::Option::Some(__slot) = __mem
                .get_mut(__o..__o + __fd.size() as usize)
            {
                ::safetynet::VmLayout::marshal::<B>(&self.tag, __slot);
            }
        }
    }
    fn unmarshal<B: ::safetynet::ByteOrder>(__mem: &[u8]) -> Self {
        let __f = <Self as ::safetynet::VmLayout>::LAYOUT.fields();
        Self {
            kind: {
                let __sub: &[u8] = match __f.get(0) {
                    ::core::option::Option::Some(__fd) => {
                        let __o = __fd.offset() as usize;
                        __mem.get(__o..__o + __fd.size() as usize).unwrap_or(&[])
                    }
                    ::core::option::Option::None => &[],
                };
                <u8 as ::safetynet::VmLayout>::unmarshal::<B>(__sub)
            },
            header: {
                let __sub: &[u8] = match __f.get(1) {
                    ::core::option::Option::Some(__fd) => {
                        let __o = __fd.offset() as usize;
                        __mem.get(__o..__o + __fd.size() as usize).unwrap_or(&[])
                    }
                    ::core::option::Option::None => &[],
                };
                <Header as ::safetynet::VmLayout>::unmarshal::<B>(__sub)
            },
            len: {
                let __sub: &[u8] = match __f.get(2) {
                    ::core::option::Option::Some(__fd) => {
                        let __o = __fd.offset() as usize;
                        __mem.get(__o..__o + __fd.size() as usize).unwrap_or(&[])
                    }
                    ::core::option::Option::None => &[],
                };
                <u16 as ::safetynet::VmLayout>::unmarshal::<B>(__sub)
            },
            tag: {
                let __sub: &[u8] = match __f.get(3) {
                    ::core::option::Option::Some(__fd) => {
                        let __o = __fd.offset() as usize;
                        __mem.get(__o..__o + __fd.size() as usize).unwrap_or(&[])
                    }
                    ::core::option::Option::None => &[],
                };
                <u64 as ::safetynet::VmLayout>::unmarshal::<B>(__sub)
            },
        }
    }
}
fn main() {}
