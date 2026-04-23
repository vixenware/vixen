use std::sync::Arc;

use cranelift_jit::JITModule;
use vx_jit_types::{StrictFunction, StrictType, StringOwnership};

use crate::codegen::CompileError;

/// A typed argument for calling a CompiledFragment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FragmentArg {
    U64(u64),
    Bool(bool),
    String(String),
}

/// The typed result from calling a CompiledFragment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FragmentResult {
    U64(u64),
    Bool(bool),
    String(String),
}

/// A compiled strict function. Owns the JIT module so generated code is never
/// dropped while the fragment is callable.
pub struct CompiledFragment {
    pub(crate) func_ptr: *const u8,
    pub(crate) return_type: StrictType,
    pub(crate) return_ownership: StringOwnership,
    pub(crate) param_types: Vec<StrictType>,
    pub(crate) _literal_storage: Vec<Box<[u8]>>,
    // Keeps the JIT module (and therefore the generated code) alive.
    _module: Arc<JITModule>,
}

// SAFETY: The JIT module's code is in executable memory that doesn't move.
// The func_ptr is valid for the lifetime of the Arc<JITModule>.
unsafe impl Send for CompiledFragment {}
unsafe impl Sync for CompiledFragment {}

impl CompiledFragment {
    pub(crate) fn new(
        func_ptr: *const u8,
        func: &StrictFunction,
        module: Arc<JITModule>,
        literal_storage: Vec<Box<[u8]>>,
    ) -> Self {
        Self {
            func_ptr,
            return_type: func.return_type,
            return_ownership: func.return_ownership,
            param_types: func.params.iter().map(|p| p.ty).collect(),
            _literal_storage: literal_storage,
            _module: module,
        }
    }

    /// Call the compiled function with typed arguments, returning a typed result.
    ///
    /// String arguments are passed as (ptr, len) borrowed slices.
    /// Owned string results are copied into a new String and the JIT-allocated
    /// buffer is freed.
    pub fn call(&self, args: &[FragmentArg]) -> Result<FragmentResult, CompileError> {
        if args.len() != self.param_types.len() {
            return Err(CompileError::ArityMismatch {
                expected: self.param_types.len(),
                actual: args.len(),
            });
        }

        // Flatten args to words.
        let mut words: Vec<u64> = Vec::new();
        for (i, (arg, &ty)) in args.iter().zip(self.param_types.iter()).enumerate() {
            match (arg, ty) {
                (FragmentArg::U64(v), StrictType::U64) => words.push(*v),
                (FragmentArg::Bool(v), StrictType::Bool) => words.push(u64::from(*v)),
                (FragmentArg::String(s), StrictType::String) => {
                    words.push(s.as_ptr() as u64);
                    words.push(s.len() as u64);
                }
                _ => {
                    return Err(CompileError::ArgTypeMismatch { index: i });
                }
            }
        }

        // Determine return word count.
        let ret_words = match self.return_type {
            StrictType::U64 | StrictType::Bool => 1,
            StrictType::String => 2,
        };

        // Call the function.
        let result = unsafe { call_words(self.func_ptr, &words, ret_words) };

        // Lift result.
        match self.return_type {
            StrictType::U64 => Ok(FragmentResult::U64(result[0])),
            StrictType::Bool => Ok(FragmentResult::Bool(result[0] != 0)),
            StrictType::String => {
                let ptr = result[0] as *const u8;
                let len = result[1] as usize;
                let s = unsafe {
                    let bytes = std::slice::from_raw_parts(ptr, len);
                    let s = std::str::from_utf8(bytes)
                        .map_err(|_| CompileError::InvalidUtf8)?
                        .to_owned();
                    // Free owned allocations after copying.
                    if self.return_ownership == StringOwnership::Owned {
                        std::alloc::dealloc(
                            ptr as *mut u8,
                            std::alloc::Layout::from_size_align(len, 1).unwrap(),
                        );
                    }
                    s
                };
                Ok(FragmentResult::String(s))
            }
        }
    }

    #[cfg(test)]
    pub(crate) fn literal_storage_len(&self) -> usize {
        self._literal_storage.len()
    }
}

/// Dispatch a call to a raw function pointer with a variable number of u64
/// arguments, returning up to two u64 words.
unsafe fn call_words(func_ptr: *const u8, args: &[u64], ret_words: usize) -> [u64; 2] {
    macro_rules! dispatch {
        ($(($n:literal, ($($a:ident),*)))*) => {
            match (args.len(), ret_words) {
                $(
                    ($n, 1) => {
                        let [$($a),*] = args else { unreachable!() };
                        let f: unsafe extern "C" fn($( $a: u64 ),*) -> u64 =
                            unsafe { std::mem::transmute(func_ptr) };
                        [unsafe { f($(*$a),*) }, 0]
                    }
                    ($n, 2) => {
                        let [$($a),*] = args else { unreachable!() };
                        let f: unsafe extern "C" fn($( $a: u64 ),*) -> (u64, u64) =
                            unsafe { std::mem::transmute(func_ptr) };
                        let (a, b) = unsafe { f($(*$a),*) };
                        [a, b]
                    }
                )*
                _ => panic!("unsupported call shape: {} args, {} ret words", args.len(), ret_words),
            }
        };
    }

    dispatch!((0, ())(1, (a0))(2, (a0, a1))(3, (a0, a1, a2))(
        4,
        (a0, a1, a2, a3)
    )(5, (a0, a1, a2, a3, a4))(6, (a0, a1, a2, a3, a4, a5))(
        7,
        (a0, a1, a2, a3, a4, a5, a6)
    )(8, (a0, a1, a2, a3, a4, a5, a6, a7)))
}
