use std::os::raw::{c_char, c_int};
use std::sync::OnceLock;

#[repr(C)]
pub struct libraw_data_t {
    _private: [u8; 0],
}

#[repr(C)]
pub struct LibRawProcessedImage {
    pub type_: c_int,
    pub height: u16,
    pub width: u16,
    pub colors: u16,
    pub bits: u16,
    pub data_size: u32,
}

pub struct LibRawApi {
    pub libraw_init: unsafe extern "C" fn(c_int) -> *mut libraw_data_t,
    pub libraw_open_buffer: unsafe extern "C" fn(*mut libraw_data_t, *const u8, usize) -> c_int,
    pub libraw_unpack: unsafe extern "C" fn(*mut libraw_data_t) -> c_int,
    pub libraw_dcraw_process: unsafe extern "C" fn(*mut libraw_data_t) -> c_int,
    pub libraw_dcraw_make_mem_image:
        unsafe extern "C" fn(*mut libraw_data_t, *mut c_int) -> *mut LibRawProcessedImage,
    pub libraw_dcraw_clear_mem: unsafe extern "C" fn(*mut LibRawProcessedImage),
    pub libraw_close: unsafe extern "C" fn(*mut libraw_data_t),
    pub libraw_strerror: unsafe extern "C" fn(c_int) -> *const c_char,
    pub libraw_set_output_bps: unsafe extern "C" fn(*mut libraw_data_t, c_int) -> c_int,
    pub libraw_set_output_color: unsafe extern "C" fn(*mut libraw_data_t, c_int) -> c_int,
    pub libraw_set_no_auto_bright: unsafe extern "C" fn(*mut libraw_data_t, c_int) -> c_int,
}

static API: OnceLock<Result<LibRawApi, anyhow::Error>> = OnceLock::new();

pub fn get_api() -> anyhow::Result<&'static LibRawApi> {
    API.get_or_init(|| {
        let lib = crate::init_libraw::get_lib()?;
        unsafe {
            let s_init: libloading::Symbol<unsafe extern "C" fn(c_int) -> *mut libraw_data_t> =
                lib.get(b"libraw_init\0").map_err(|e| anyhow::anyhow!(e))?;
            let s_open_buf: libloading::Symbol<
                unsafe extern "C" fn(*mut libraw_data_t, *const u8, usize) -> c_int,
            > = lib
                .get(b"libraw_open_buffer\0")
                .map_err(|e| anyhow::anyhow!(e))?;
            let s_unpack: libloading::Symbol<unsafe extern "C" fn(*mut libraw_data_t) -> c_int> =
                lib.get(b"libraw_unpack\0")
                    .map_err(|e| anyhow::anyhow!(e))?;
            let s_process: libloading::Symbol<unsafe extern "C" fn(*mut libraw_data_t) -> c_int> =
                lib.get(b"libraw_dcraw_process\0")
                    .map_err(|e| anyhow::anyhow!(e))?;
            let s_make_mem: libloading::Symbol<
                unsafe extern "C" fn(*mut libraw_data_t, *mut c_int) -> *mut LibRawProcessedImage,
            > = lib
                .get(b"libraw_dcraw_make_mem_image\0")
                .map_err(|e| anyhow::anyhow!(e))?;
            let s_clear_mem: libloading::Symbol<unsafe extern "C" fn(*mut LibRawProcessedImage)> =
                lib.get(b"libraw_dcraw_clear_mem\0")
                    .map_err(|e| anyhow::anyhow!(e))?;
            let s_close: libloading::Symbol<unsafe extern "C" fn(*mut libraw_data_t)> =
                lib.get(b"libraw_close\0").map_err(|e| anyhow::anyhow!(e))?;
            let s_strerror: libloading::Symbol<unsafe extern "C" fn(c_int) -> *const c_char> = lib
                .get(b"libraw_strerror\0")
                .map_err(|e| anyhow::anyhow!(e))?;
            let s_set_bps: libloading::Symbol<
                unsafe extern "C" fn(*mut libraw_data_t, c_int) -> c_int,
            > = lib
                .get(b"libraw_set_output_bps\0")
                .map_err(|e| anyhow::anyhow!(e))?;
            let s_set_color: libloading::Symbol<
                unsafe extern "C" fn(*mut libraw_data_t, c_int) -> c_int,
            > = lib
                .get(b"libraw_set_output_color\0")
                .map_err(|e| anyhow::anyhow!(e))?;
            let s_set_no_auto: libloading::Symbol<
                unsafe extern "C" fn(*mut libraw_data_t, c_int) -> c_int,
            > = lib
                .get(b"libraw_set_no_auto_bright\0")
                .map_err(|e| anyhow::anyhow!(e))?;

            let api = LibRawApi {
                libraw_init: *s_init,
                libraw_open_buffer: *s_open_buf,
                libraw_unpack: *s_unpack,
                libraw_dcraw_process: *s_process,
                libraw_dcraw_make_mem_image: *s_make_mem,
                libraw_dcraw_clear_mem: *s_clear_mem,
                libraw_close: *s_close,
                libraw_strerror: *s_strerror,
                libraw_set_output_bps: *s_set_bps,
                libraw_set_output_color: *s_set_color,
                libraw_set_no_auto_bright: *s_set_no_auto,
            };
            Ok(api)
        }
    })
    .as_ref()
    .map_err(|e| anyhow::anyhow!(e))
}
