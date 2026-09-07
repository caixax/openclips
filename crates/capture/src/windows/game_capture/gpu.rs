//! Frames handed to the pipeline as Direct3D 11 textures. GStreamer's D3D11
//! helper library (`gstd3d11-1.0`) can wrap a foreign `ID3D11Device` and a
//! foreign texture into a `GstD3D11Device` and `GstD3D11Memory`; with the
//! device published as a pipeline context, `d3d11upload` passes the buffers
//! through and `d3d11convert` and the encoder work on the same device, so a
//! captured frame never leaves the GPU. The library has no Rust binding, so
//! the handful of entry points used here are declared by hand; the ABI is
//! the stable public one since GStreamer 1.22.

use std::ffi::c_void;

use gstreamer as gst;
use gstreamer::glib::translate::from_glib_full;
use windows::Win32::Graphics::Direct3D11::{ID3D11Device, ID3D11Texture2D};
use windows::core::Interface;

use crate::error::CaptureError;

mod ffi {
    use std::ffi::c_void;

    use gstreamer as gst;

    #[repr(C)]
    pub struct GstD3D11Device {
        _private: [u8; 0],
    }

    // Linked through the pkg-config probe in build.rs.
    unsafe extern "C" {
        pub fn gst_d3d11_device_new_wrapped(device: *mut c_void) -> *mut GstD3D11Device;
        pub fn gst_d3d11_context_new(device: *mut GstD3D11Device) -> *mut gst::ffi::GstContext;
        pub fn gst_d3d11_device_lock(device: *mut GstD3D11Device);
        pub fn gst_d3d11_device_unlock(device: *mut GstD3D11Device);
        /// A null allocator selects the library's own; the texture is
        /// referenced by the memory and `notify(user_data)` runs when the
        /// memory is freed.
        pub fn gst_d3d11_allocator_alloc_wrapped(
            allocator: *mut c_void,
            device: *mut GstD3D11Device,
            texture: *mut c_void,
            size: usize,
            user_data: *mut c_void,
            notify: Option<unsafe extern "C" fn(*mut c_void)>,
        ) -> *mut gst::ffi::GstMemory;
    }
}

/// A `GstD3D11Device` wrapping our own `ID3D11Device`. Reference counted on
/// the GStreamer side; this handle holds one reference.
pub struct Device {
    raw: *mut ffi::GstD3D11Device,
}

// A GstObject is safe to share between threads; the device serialises use
// of its immediate context through `lock`.
unsafe impl Send for Device {}
unsafe impl Sync for Device {}

impl Device {
    pub fn wrap(device: &ID3D11Device) -> Result<Self, CaptureError> {
        // SAFETY: a valid COM pointer; the library takes its own reference.
        let raw = unsafe { ffi::gst_d3d11_device_new_wrapped(device.as_raw()) };
        if raw.is_null() {
            return Err(CaptureError::GameCapture(
                "GStreamer could not adopt the capture device".to_owned(),
            ));
        }
        Ok(Self { raw })
    }

    /// The context that tells every D3D11 element in a pipeline to use this
    /// device.
    pub fn context(&self) -> Result<gst::Context, CaptureError> {
        // SAFETY: `raw` is a live device; the context is a new full reference.
        let ptr = unsafe { ffi::gst_d3d11_context_new(self.raw) };
        if ptr.is_null() {
            return Err(CaptureError::GameCapture(
                "GStreamer could not create the device context".to_owned(),
            ));
        }
        // SAFETY: a full reference to a valid GstContext.
        Ok(unsafe { from_glib_full(ptr) })
    }

    /// Holds the device lock, which every user of its immediate context
    /// (GStreamer's elements included) takes around D3D calls.
    pub fn lock(&self) -> Locked<'_> {
        // SAFETY: `raw` is a live device.
        unsafe { ffi::gst_d3d11_device_lock(self.raw) };
        Locked { device: self }
    }

    /// Wraps `texture` into GStreamer memory of `size` bytes. `on_free` runs
    /// on whichever thread drops the last reference to the memory.
    pub fn wrap_texture(
        &self,
        texture: &ID3D11Texture2D,
        size: usize,
        on_free: Box<dyn FnOnce() + Send>,
    ) -> Result<gst::Memory, CaptureError> {
        let user_data = Box::into_raw(Box::new(on_free)) as *mut c_void;
        // SAFETY: valid device and texture pointers; `user_data` is a boxed
        // closure handed back to `release` exactly once by the library.
        let ptr = unsafe {
            ffi::gst_d3d11_allocator_alloc_wrapped(
                std::ptr::null_mut(),
                self.raw,
                texture.as_raw(),
                size,
                user_data,
                Some(release),
            )
        };
        if ptr.is_null() {
            // SAFETY: the library did not take the closure; reclaim it.
            drop(unsafe { Box::from_raw(user_data as *mut Box<dyn FnOnce() + Send>) });
            return Err(CaptureError::GameCapture(
                "GStreamer could not wrap a frame texture".to_owned(),
            ));
        }
        // SAFETY: a full reference to a valid GstMemory.
        Ok(unsafe { from_glib_full(ptr) })
    }
}

impl Drop for Device {
    fn drop(&mut self) {
        // SAFETY: releasing the reference `wrap` obtained.
        unsafe { gst::ffi::gst_object_unref(self.raw as *mut gst::ffi::GstObject) };
    }
}

/// The device lock, released on drop.
pub struct Locked<'a> {
    device: &'a Device,
}

impl Drop for Locked<'_> {
    fn drop(&mut self) {
        // SAFETY: the lock was taken by `Device::lock`.
        unsafe { ffi::gst_d3d11_device_unlock(self.device.raw) };
    }
}

unsafe extern "C" fn release(user_data: *mut c_void) {
    if user_data.is_null() {
        return;
    }
    // SAFETY: the pointer came from `Box::into_raw` in `wrap_texture` and
    // the library calls this once.
    let on_free = unsafe { Box::from_raw(user_data as *mut Box<dyn FnOnce() + Send>) };
    on_free();
}
