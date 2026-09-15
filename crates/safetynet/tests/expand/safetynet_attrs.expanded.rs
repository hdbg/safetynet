#![allow(dead_code)]
use safetynet::safetynet;
#[allow(dead_code)]
#[must_use]
fn __sn_ref_doubled(x: u32) -> u32 {
    x * 2
}
#[must_use]
fn doubled(x: u32) -> u32 {
    let mut __sn_input = [0u8; 4];
    if let ::core::option::Option::Some(__sn_slot) = __sn_input.get_mut(0..) {
        ::safetynet::VmLayout::marshal::<::safetynet::Le>(&x, __sn_slot);
    }
    let __sn_layout = match ::safetynet::Layout::new(::safetynet::image::Sizes {
        input: 4u32,
        scratch: 0,
        stack: 4096u32,
    }) {
        ::core::option::Option::Some(__sn_layout) => __sn_layout,
        ::core::option::Option::None => {
            ::core::panicking::panic_fmt(
                format_args!("safetynet: the image does not fit"),
            );
        }
    };
    let mut __sn_image = ::safetynet::Image::new(__sn_layout);
    if __sn_image.write(::safetynet::Region::Input, &__sn_input).is_none() {
        {
            ::core::panicking::panic_fmt(
                format_args!("safetynet: the input does not fit"),
            );
        };
    }
    let __sn_program = match __sn_program_doubled().finalize(&__sn_layout) {
        ::core::result::Result::Ok(__sn_program) => __sn_program,
        ::core::result::Result::Err(__sn_error) => {
            ::core::panicking::panic_fmt(format_args!("safetynet: {0}", __sn_error));
        }
    };
    let mut __sn_vm = match ::safetynet::Vm::<::safetynet::Le>::new(__sn_image)
        .run(&__sn_program, 1000000u64)
    {
        ::core::result::Result::Ok(__sn_vm) => __sn_vm,
        ::core::result::Result::Err(__sn_trap) => {
            ::core::panicking::panic_fmt(format_args!("safetynet: {0}", __sn_trap));
        }
    };
    let __sn_result = match __sn_vm.pop() {
        ::core::result::Result::Ok(__sn_word) => __sn_word,
        ::core::result::Result::Err(__sn_trap) => {
            ::core::panicking::panic_fmt(format_args!("safetynet: {0}", __sn_trap));
        }
    };
    <u32 as ::safetynet::VmValue>::from_word(__sn_result)
}
fn __sn_program_doubled() -> ::safetynet::Artifact<::safetynet::Le> {
    {
        const CODE: &[u8] = &::safetynet::link::<
            10,
        >([2u8, 0u8, 0u8, 0u8, 0u8, 14u8, 1u8, 2u8, 21u8, 0u8], &[]);
        ::safetynet::Artifact::<
            ::safetynet::Le,
        >::new(
            CODE,
            ::safetynet::FrameSize::new(0u16).unwrap_or_default(),
            ::alloc::boxed::box_assume_init_into_vec_unsafe(
                ::alloc::intrinsics::write_box_via_move(
                    ::alloc::boxed::Box::new_uninit(),
                    [
                        ::safetynet::Reloc::region_base::<
                            ::safetynet::encoding::Packed<::safetynet::Le>,
                        >(0usize, ::safetynet::Region::Input),
                    ],
                ),
            ),
        )
    }
}
const _: fn() = || {
    fn __sn_assert_vm_value<__T: ::safetynet::VmValue>() {}
    fn __sn_assert_vm_layout<__T: ::safetynet::VmLayout>() {}
    __sn_assert_vm_value::<u32>();
    __sn_assert_vm_value::<u32>();
};
fn main() {}
