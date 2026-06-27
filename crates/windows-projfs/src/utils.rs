use windows::{Win32::Foundation::ERROR_IO_INCOMPLETE, core::HRESULT};

pub fn io_result_to_hresult(error: std::io::Error) -> HRESULT {
    HRESULT::from_win32(error.raw_os_error().unwrap_or(ERROR_IO_INCOMPLETE.0 as i32) as u32)
}
