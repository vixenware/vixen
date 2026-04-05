#![deny(clippy::all)]
#![allow(clippy::blocks_in_conditions)]

mod error;
pub use error::*;

mod source;
pub use source::*;

mod fs;
pub use fs::*;

mod callback_data;
use callback_data::*;

mod aligned_buffer;
mod library;
mod utils;
