//! Runtime/strict-fragment ABI bridge for compiled strict fragments.
//!
//! This is the narrow current bridge between host-side values and the compiled
//! Kajit calling convention. String returns may be surfaced either as raw
//! frame-valid `[ptr, len]` payloads or as copied owned strings at the runtime
//! boundary.

use std::str::Utf8Error;

use kajit::CompiledFunction;
use thiserror::Error;

use crate::{
    KajitBackendError, StrictFragmentFunction, StrictFragmentType, StrictFragmentValue,
    StrictFragmentValueKind,
};

pub const MAX_STRICT_FRAGMENT_ABI_WORDS: usize = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UnmanagedStrictFragmentString {
    pub ptr: u64,
    pub len: u64,
}

impl UnmanagedStrictFragmentString {
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
    /// The backing storage is treated as frame-valid and borrowed for the
    /// duration of the boundary crossing. This helper performs the required
    /// promotion into an owned runtime string.
    pub unsafe fn to_owned_string(&self) -> Result<String, Utf8Error> {
        let bytes = unsafe { self.as_bytes() };
        std::str::from_utf8(bytes).map(str::to_owned)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CompiledStrictFragmentOutput {
    Value(StrictFragmentValue),
    UnmanagedString(UnmanagedStrictFragmentString),
}

#[derive(Debug, Error)]
pub enum StrictFragmentAbiError {
    #[error("arity mismatch: expected {expected} args, got {actual}")]
    ArityMismatch { expected: usize, actual: usize },
    #[error("parameter {index} expected {expected:?}, got {found:?}")]
    ArgumentTypeMismatch {
        index: usize,
        expected: StrictFragmentType,
        found: StrictFragmentValueKind,
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

pub fn compile_strict_fragment(
    function: &StrictFragmentFunction,
) -> Result<CompiledFunction, StrictFragmentAbiError> {
    Ok(crate::kajit_backend::compile(function)?)
}

pub unsafe fn execute_compiled_unmanaged_strict_fragment(
    function: &StrictFragmentFunction,
    compiled: &CompiledFunction,
    args: &[StrictFragmentValue],
) -> Result<CompiledStrictFragmentOutput, StrictFragmentAbiError> {
    let arg_words = flatten_args(function, args)?;
    let ret_words = return_word_count(function.return_type);
    if arg_words.len() > MAX_STRICT_FRAGMENT_ABI_WORDS {
        return Err(StrictFragmentAbiError::TooManyAbiWords {
            actual: arg_words.len(),
            max: MAX_STRICT_FRAGMENT_ABI_WORDS,
        });
    }

    let words = unsafe { call_compiled_words(compiled, &arg_words, ret_words) };
    Ok(match function.return_type {
        StrictFragmentType::U64 => {
            CompiledStrictFragmentOutput::Value(StrictFragmentValue::U64(words[0]))
        }
        StrictFragmentType::Bool => {
            CompiledStrictFragmentOutput::Value(StrictFragmentValue::Bool(match words[0] {
                0 => false,
                1 => true,
                other => return Err(StrictFragmentAbiError::InvalidBoolWord(other)),
            }))
        }
        StrictFragmentType::String => {
            CompiledStrictFragmentOutput::UnmanagedString(UnmanagedStrictFragmentString {
                ptr: words[0],
                len: words[1],
            })
        }
    })
}

/// Executes a compiled strict fragment and promotes frame-valid string returns
/// into owned Rust strings for use outside the strict-fragment boundary.
pub unsafe fn execute_compiled_copying_strict_fragment_strings(
    function: &StrictFragmentFunction,
    compiled: &CompiledFunction,
    args: &[StrictFragmentValue],
) -> Result<StrictFragmentValue, StrictFragmentAbiError> {
    match unsafe { execute_compiled_unmanaged_strict_fragment(function, compiled, args)? } {
        CompiledStrictFragmentOutput::Value(value) => Ok(value),
        CompiledStrictFragmentOutput::UnmanagedString(value) => {
            Ok(StrictFragmentValue::String(unsafe {
                value.to_owned_string()?
            }))
        }
    }
}

fn flatten_args(
    function: &StrictFragmentFunction,
    args: &[StrictFragmentValue],
) -> Result<Vec<u64>, StrictFragmentAbiError> {
    if function.params.len() != args.len() {
        return Err(StrictFragmentAbiError::ArityMismatch {
            expected: function.params.len(),
            actual: args.len(),
        });
    }

    let mut words = Vec::new();
    for (index, (param, value)) in function.params.iter().zip(args).enumerate() {
        match (&param.ty, value) {
            (StrictFragmentType::U64, StrictFragmentValue::U64(value)) => words.push(*value),
            (StrictFragmentType::Bool, StrictFragmentValue::Bool(value)) => {
                words.push(u64::from(*value))
            }
            (StrictFragmentType::String, StrictFragmentValue::String(value)) => {
                words.push(value.as_ptr() as u64);
                words.push(value.len() as u64);
            }
            (expected, found) => {
                return Err(StrictFragmentAbiError::ArgumentTypeMismatch {
                    index,
                    expected: *expected,
                    found: found.kind(),
                });
            }
        }
    }

    Ok(words)
}

const fn return_word_count(ty: StrictFragmentType) -> usize {
    match ty {
        StrictFragmentType::U64 | StrictFragmentType::Bool => 1,
        StrictFragmentType::String => 2,
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
