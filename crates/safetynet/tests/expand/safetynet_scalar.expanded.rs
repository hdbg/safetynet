#![allow(dead_code)]
use safetynet::safetynet;
#[allow(dead_code)]
fn __sn_ref_inc(x: u32) -> u32 {
    x + 1
}
fn inc(x: u32) -> u32 {
    let mut __sn_fixed = [0u8; 4];
    let mut __sn_tail = ::safetynet::Tail::new(4);
    if let ::core::option::Option::Some(__sn_slot) = __sn_fixed.get_mut(0..) {
        ::safetynet::VmLayout::marshal::<::safetynet::Le>(&x, __sn_slot, &mut __sn_tail);
    }
    let mut __sn_input = __sn_fixed.to_vec();
    __sn_input.extend_from_slice(__sn_tail.as_slice());
    let __sn_input_len = match u32::try_from(__sn_input.len()) {
        ::core::result::Result::Ok(__sn_len) => __sn_len,
        ::core::result::Result::Err(_) => {
            ::safetynet::__private::fail(::safetynet::__private::Failure::InputTooLarge)
        }
    };
    let __sn_layout = match ::safetynet::Layout::new(::safetynet::image::Sizes {
        input: __sn_input_len,
        scratch: 0,
        stack: 4096u32,
    }) {
        ::core::option::Option::Some(__sn_layout) => __sn_layout,
        ::core::option::Option::None => {
            ::safetynet::__private::fail(
                ::safetynet::__private::Failure::ImageDoesNotFit,
            )
        }
    };
    let mut __sn_image = ::safetynet::Image::new(__sn_layout);
    if __sn_image.write(::safetynet::Region::Input, &__sn_input).is_none() {
        ::safetynet::__private::fail(::safetynet::__private::Failure::InputDoesNotFit);
    }
    let __sn_program = match __sn_program_inc().finalize(&__sn_layout) {
        ::core::result::Result::Ok(__sn_program) => __sn_program,
        ::core::result::Result::Err(__sn_error) => {
            ::safetynet::__private::fail(
                ::safetynet::__private::Failure::NotFinal(__sn_error),
            )
        }
    };
    let mut __sn_vm = match ::safetynet::Vm::<::safetynet::Le>::new(__sn_image)
        .run(&__sn_program, 1000000u64)
    {
        ::core::result::Result::Ok(__sn_vm) => __sn_vm,
        ::core::result::Result::Err(__sn_trap) => {
            ::safetynet::__private::fail(
                ::safetynet::__private::Failure::Trap(__sn_trap),
            )
        }
    };
    let __sn_result = match __sn_vm.pop() {
        ::core::result::Result::Ok(__sn_word) => __sn_word,
        ::core::result::Result::Err(__sn_trap) => {
            ::safetynet::__private::fail(
                ::safetynet::__private::Failure::Trap(__sn_trap),
            )
        }
    };
    <u32 as ::safetynet::VmValue>::from_word(__sn_result)
}
fn __sn_program_inc() -> ::safetynet::Artifact<::safetynet::Le> {
    {
        const CODE: &[u8] = &::safetynet::link::<
            16,
        >(
            [
                2u8, 0u8, 0u8, 0u8, 0u8, 14u8, 1u8, 1u8, 19u8, 2u8, 255u8, 255u8, 255u8,
                255u8, 26u8, 0u8,
            ],
            &[],
        );
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
