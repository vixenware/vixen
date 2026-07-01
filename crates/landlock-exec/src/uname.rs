/*
Copyright (c) 2016 rust-uname Developers

Permission is hereby granted, free of charge, to any
person obtaining a copy of this software and associated
documentation files (the "Software"), to deal in the
Software without restriction, including without
limitation the rights to use, copy, modify, merge,
publish, distribute, sublicense, and/or sell copies of
the Software, and to permit persons to whom the Software
is furnished to do so, subject to the following
conditions:

The above copyright notice and this permission notice
shall be included in all copies or substantial portions
of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF
ANY KIND, EXPRESS OR IMPLIED, INCLUDING BUT NOT LIMITED
TO THE WARRANTIES OF MERCHANTABILITY, FITNESS FOR A
PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT
SHALL THE AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY
CLAIM, DAMAGES OR OTHER LIABILITY, WHETHER IN AN ACTION
OF CONTRACT, TORT OR OTHERWISE, ARISING FROM, OUT OF OR
IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER
DEALINGS IN THE SOFTWARE.
*/

// The upstream version of this crate is at https://crates.io/crates/uname,
// decided to copy-paste here to reduce innecessary dependencies.

extern crate libc;

use std::ffi::CStr;
use std::io;

use libc::{c_char, utsname};

#[derive(Debug)]
pub struct Info {
    pub sysname: String,
    pub nodename: String,
    pub release: String,
    pub version: String,
    pub machine: String,
    _priv: (),
}

impl Info {
    pub fn new() -> io::Result<Self> {
        let mut n = unsafe { std::mem::zeroed() };
        let r = unsafe { libc::uname(&mut n) };
        if r == 0 {
            Ok(From::from(n))
        } else {
            Err(io::Error::last_os_error())
        }
    }
}

#[inline]
fn to_cstr(buf: &[c_char]) -> &CStr {
    unsafe { CStr::from_ptr(buf.as_ptr()) }
}

impl From<utsname> for Info {
    fn from(x: utsname) -> Self {
        Info {
            sysname: to_cstr(&x.sysname[..]).to_string_lossy().into_owned(),
            nodename: to_cstr(&x.nodename[..]).to_string_lossy().into_owned(),
            release: to_cstr(&x.release[..]).to_string_lossy().into_owned(),
            version: to_cstr(&x.version[..]).to_string_lossy().into_owned(),
            machine: to_cstr(&x.machine[..]).to_string_lossy().into_owned(),
            _priv: (),
        }
    }
}

pub fn uname() -> io::Result<Info> {
    Info::new()
}
