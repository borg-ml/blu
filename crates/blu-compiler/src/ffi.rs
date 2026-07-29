use core::ptr::NonNull;
use std::ffi::{c_char, c_int, c_void};

const STATUS_OK: c_int = 0;
const STATUS_ALLOCATION: c_int = 1;
const STATUS_EXCEPTION: c_int = 2;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Error {
    Allocation,
    Exception,
    Contract {
        status: c_int,
        output_is_null: bool,
        output_size: usize,
    },
    TooLarge {
        actual: usize,
        limit: usize,
    },
}

struct NativeBuffer {
    pointer: NonNull<c_char>,
    len: usize,
}

impl NativeBuffer {
    fn try_to_vec(&self) -> Result<Vec<u8>, Error> {
        // SAFETY: The shim returns a live allocation initialized for `len`
        // bytes. `compile` checks that `len <= isize::MAX` before construction.
        let bytes =
            unsafe { core::slice::from_raw_parts(self.pointer.as_ptr().cast::<u8>(), self.len) };
        let mut owned = Vec::new();
        owned
            .try_reserve_exact(self.len)
            .map_err(|_| Error::Allocation)?;
        owned.extend_from_slice(bytes);
        Ok(owned)
    }
}

impl Drop for NativeBuffer {
    fn drop(&mut self) {
        // SAFETY: The pointer came from `blu_luau_compile`, is still owned by
        // this guard, and the paired shim deallocator has not been called.
        unsafe { blu_luau_free(self.pointer.as_ptr().cast()) };
    }
}

pub(super) fn compile(
    source: &[u8],
    optimization_level: u8,
    debug_level: u8,
    type_info_level: u8,
    coverage_level: u8,
    max_output_size: usize,
) -> Result<Vec<u8>, Error> {
    let mut output = core::ptr::null_mut();
    let mut output_size = 0;

    // SAFETY: The source is readable for its reported length and both output
    // pointers remain valid for the call. The noexcept shim contains all C++
    // exceptions and reports ownership through `output`.
    let status = unsafe {
        blu_luau_compile(
            source.as_ptr().cast(),
            source.len(),
            c_int::from(optimization_level),
            c_int::from(debug_level),
            c_int::from(type_info_level),
            c_int::from(coverage_level),
            &raw mut output,
            &raw mut output_size,
        )
    };

    finish(status, output, output_size, max_output_size)
}

fn finish(
    status: c_int,
    output: *mut c_char,
    output_size: usize,
    max_output_size: usize,
) -> Result<Vec<u8>, Error> {
    let buffer = NonNull::new(output).map(|pointer| NativeBuffer {
        pointer,
        len: output_size,
    });

    if status != STATUS_OK {
        if buffer.is_some() || output_size != 0 {
            return Err(Error::Contract {
                status,
                output_is_null: buffer.is_none(),
                output_size,
            });
        }
        return match status {
            STATUS_ALLOCATION => Err(Error::Allocation),
            STATUS_EXCEPTION => Err(Error::Exception),
            _ => Err(Error::Contract {
                status,
                output_is_null: true,
                output_size,
            }),
        };
    }

    let Some(buffer) = buffer else {
        return Err(Error::Contract {
            status,
            output_is_null: true,
            output_size,
        });
    };

    let limit = max_output_size.min(isize::MAX as usize);
    if output_size > limit {
        return Err(Error::TooLarge {
            actual: output_size,
            limit,
        });
    }

    buffer.try_to_vec()
}

#[cfg(test)]
pub(super) fn test_exception(kind: c_int) -> Result<(), Error> {
    // SAFETY: The test entrypoint accepts an integer and is declared noexcept.
    let status = unsafe { blu_luau_test_exception_status(kind) };
    match status {
        STATUS_OK => Ok(()),
        STATUS_ALLOCATION => Err(Error::Allocation),
        STATUS_EXCEPTION => Err(Error::Exception),
        _ => Err(Error::Contract {
            status,
            output_is_null: true,
            output_size: 0,
        }),
    }
}

unsafe extern "C" {
    fn blu_luau_compile(
        source: *const c_char,
        source_size: usize,
        optimization_level: c_int,
        debug_level: c_int,
        type_info_level: c_int,
        coverage_level: c_int,
        output: *mut *mut c_char,
        output_size: *mut usize,
    ) -> c_int;
    fn blu_luau_free(pointer: *mut c_void);

    #[cfg(test)]
    fn blu_luau_test_exception_status(kind: c_int) -> c_int;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_incoherent_native_outputs() {
        assert_eq!(
            finish(STATUS_OK, core::ptr::null_mut(), 0, usize::MAX),
            Err(Error::Contract {
                status: STATUS_OK,
                output_is_null: true,
                output_size: 0,
            })
        );
        assert_eq!(
            finish(STATUS_ALLOCATION, core::ptr::null_mut(), 7, usize::MAX),
            Err(Error::Contract {
                status: STATUS_ALLOCATION,
                output_is_null: true,
                output_size: 7,
            })
        );
    }
}
