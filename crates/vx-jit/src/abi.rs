//! Runtime/kernel ABI bridge for compiled strict kernels.
//!
//! This is the narrow current bridge between host-side values and the compiled
//! Kajit calling convention. It is intentionally explicit about unmanaged
//! string returns: the bridge can copy them out, but it cannot yet prove
//! ownership of the returned backing storage.

use std::str::Utf8Error;

use kajit::CompiledFunction;
use thiserror::Error;

use crate::{KajitBackendError, KernelFunction, KernelType, KernelValue, KernelValueKind};

pub const MAX_ABI_WORDS: usize = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UnmanagedKernelString {
    pub ptr: u64,
    pub len: u64,
}

impl UnmanagedKernelString {
    pub fn len(&self) -> usize {
        self.len as usize
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Caller must ensure the returned memory remains valid for the borrow.
    pub unsafe fn as_bytes<'a>(&self) -> &'a [u8] {
        unsafe { std::slice::from_raw_parts(self.ptr as *const u8, self.len()) }
    }

    /// Copies the returned string bytes into an owned Rust `String`.
    ///
    /// This does not free the underlying backing storage. That ownership story
    /// is still unresolved at the runtime/kernel boundary.
    pub unsafe fn to_owned_string(&self) -> Result<String, Utf8Error> {
        let bytes = unsafe { self.as_bytes() };
        std::str::from_utf8(bytes).map(str::to_owned)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CompiledKernelOutput {
    Value(KernelValue),
    UnmanagedString(UnmanagedKernelString),
}

#[derive(Debug, Error)]
pub enum KernelAbiError {
    #[error("arity mismatch: expected {expected} args, got {actual}")]
    ArityMismatch { expected: usize, actual: usize },
    #[error("parameter {index} expected {expected:?}, got {found:?}")]
    ArgumentTypeMismatch {
        index: usize,
        expected: KernelType,
        found: KernelValueKind,
    },
    #[error("compiled signature uses {actual} ABI words, max supported is {max}")]
    TooManyAbiWords { actual: usize, max: usize },
    #[error("bool result word must be 0 or 1, got {0}")]
    InvalidBoolWord(u64),
    #[error("compiled string result was not valid utf-8: {0}")]
    InvalidUtf8(#[from] Utf8Error),
    #[error(transparent)]
    Compile(#[from] KajitBackendError),
}

pub fn compile_kernel(function: &KernelFunction) -> Result<CompiledFunction, KernelAbiError> {
    Ok(crate::kajit_backend::compile(function)?)
}

pub unsafe fn execute_compiled_unmanaged(
    function: &KernelFunction,
    compiled: &CompiledFunction,
    args: &[KernelValue],
) -> Result<CompiledKernelOutput, KernelAbiError> {
    let arg_words = flatten_args(function, args)?;
    let ret_words = return_word_count(function.return_type);
    if arg_words.len() > MAX_ABI_WORDS {
        return Err(KernelAbiError::TooManyAbiWords {
            actual: arg_words.len(),
            max: MAX_ABI_WORDS,
        });
    }

    let words = unsafe { call_compiled_words(compiled, &arg_words, ret_words) };
    Ok(match function.return_type {
        KernelType::U64 => CompiledKernelOutput::Value(KernelValue::U64(words[0])),
        KernelType::Bool => CompiledKernelOutput::Value(KernelValue::Bool(match words[0] {
            0 => false,
            1 => true,
            other => return Err(KernelAbiError::InvalidBoolWord(other)),
        })),
        KernelType::String => CompiledKernelOutput::UnmanagedString(UnmanagedKernelString {
            ptr: words[0],
            len: words[1],
        }),
    })
}

/// Executes a compiled kernel and copies string returns into owned Rust
/// strings without attempting to free the underlying backing storage.
pub unsafe fn execute_compiled_copying_strings(
    function: &KernelFunction,
    compiled: &CompiledFunction,
    args: &[KernelValue],
) -> Result<KernelValue, KernelAbiError> {
    match unsafe { execute_compiled_unmanaged(function, compiled, args)? } {
        CompiledKernelOutput::Value(value) => Ok(value),
        CompiledKernelOutput::UnmanagedString(value) => {
            Ok(KernelValue::String(unsafe { value.to_owned_string()? }))
        }
    }
}

fn flatten_args(
    function: &KernelFunction,
    args: &[KernelValue],
) -> Result<Vec<u64>, KernelAbiError> {
    if function.params.len() != args.len() {
        return Err(KernelAbiError::ArityMismatch {
            expected: function.params.len(),
            actual: args.len(),
        });
    }

    let mut words = Vec::new();
    for (index, (param, value)) in function.params.iter().zip(args).enumerate() {
        match (&param.ty, value) {
            (KernelType::U64, KernelValue::U64(value)) => words.push(*value),
            (KernelType::Bool, KernelValue::Bool(value)) => words.push(u64::from(*value)),
            (KernelType::String, KernelValue::String(value)) => {
                words.push(value.as_ptr() as u64);
                words.push(value.len() as u64);
            }
            (expected, found) => {
                return Err(KernelAbiError::ArgumentTypeMismatch {
                    index,
                    expected: *expected,
                    found: found.kind(),
                });
            }
        }
    }

    Ok(words)
}

const fn return_word_count(ty: KernelType) -> usize {
    match ty {
        KernelType::U64 | KernelType::Bool => 1,
        KernelType::String => 2,
    }
}

unsafe fn call_compiled_words(
    compiled: &CompiledFunction,
    arg_words: &[u64],
    ret_words: usize,
) -> [u64; 2] {
    macro_rules! call_variants {
        ($(($count:literal, ($($arg:ident),*), $sig1:ty, $sig2:ty)),* $(,)?) => {
            match (arg_words.len(), ret_words) {
                $(
                    ($count, 1) => {
                        let [$($arg),*] = arg_words else { unreachable!() };
                        let f: $sig1 = unsafe { core::mem::transmute(compiled.as_ptr()) };
                        [unsafe { f($(*$arg),*) }, 0]
                    }
                )*
                $(
                    ($count, 2) => {
                        let [$($arg),*] = arg_words else { unreachable!() };
                        let f: $sig2 = unsafe { core::mem::transmute(compiled.as_ptr()) };
                        let (a, b) = unsafe { f($(*$arg),*) };
                        [a, b]
                    }
                )*
                _ => unreachable!(),
            }
        };
    }

    call_variants!(
        (0, (), unsafe extern "C" fn() -> u64, unsafe extern "C" fn() -> (u64, u64)),
        (1, (a0), unsafe extern "C" fn(u64) -> u64, unsafe extern "C" fn(u64) -> (u64, u64)),
        (2, (a0, a1), unsafe extern "C" fn(u64, u64) -> u64, unsafe extern "C" fn(u64, u64) -> (u64, u64)),
        (3, (a0, a1, a2), unsafe extern "C" fn(u64, u64, u64) -> u64, unsafe extern "C" fn(u64, u64, u64) -> (u64, u64)),
        (4, (a0, a1, a2, a3), unsafe extern "C" fn(u64, u64, u64, u64) -> u64, unsafe extern "C" fn(u64, u64, u64, u64) -> (u64, u64)),
        (5, (a0, a1, a2, a3, a4), unsafe extern "C" fn(u64, u64, u64, u64, u64) -> u64, unsafe extern "C" fn(u64, u64, u64, u64, u64) -> (u64, u64)),
        (6, (a0, a1, a2, a3, a4, a5), unsafe extern "C" fn(u64, u64, u64, u64, u64, u64) -> u64, unsafe extern "C" fn(u64, u64, u64, u64, u64, u64) -> (u64, u64)),
        (7, (a0, a1, a2, a3, a4, a5, a6), unsafe extern "C" fn(u64, u64, u64, u64, u64, u64, u64) -> u64, unsafe extern "C" fn(u64, u64, u64, u64, u64, u64, u64) -> (u64, u64)),
        (8, (a0, a1, a2, a3, a4, a5, a6, a7), unsafe extern "C" fn(u64, u64, u64, u64, u64, u64, u64, u64) -> u64, unsafe extern "C" fn(u64, u64, u64, u64, u64, u64, u64, u64) -> (u64, u64)),
    )
}
