//! The host side of the OBS capture hook handshake and the shared texture
//! reader. One [`HookSession`] drives one hooked game: it injects the signed
//! hook, negotiates through the shared control block, opens the texture the
//! hook copies every presented frame into, and hands out CPU frames.
//!
//! The frames go through an `appsrc` and `d3d11upload` in the pipeline, the
//! same route OBS uses in shared memory (compatibility) mode. This costs one
//! GPU to CPU copy per output frame but captures the game's real backbuffer,
//! so it does not drop frames when the GPU is saturated the way desktop
//! capture does.

use std::ffi::c_void;
use std::time::{Duration, Instant};

use tracing::info;
use windows::Win32::Foundation::{CloseHandle, HANDLE, HWND, WAIT_OBJECT_0};
use windows::Win32::Graphics::Direct3D::{D3D_DRIVER_TYPE_HARDWARE, D3D_FEATURE_LEVEL_11_0};
use windows::Win32::Graphics::Direct3D11::{
    D3D11_CPU_ACCESS_READ, D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_MAP_READ,
    D3D11_MAPPED_SUBRESOURCE, D3D11_SDK_VERSION, D3D11_TEXTURE2D_DESC, D3D11_USAGE_STAGING,
    D3D11CreateDevice, ID3D11Device, ID3D11DeviceContext, ID3D11Texture2D,
};
use windows::Win32::System::Memory::{
    FILE_MAP_ALL_ACCESS, MapViewOfFile, OpenFileMappingW, UnmapViewOfFile,
};
use windows::Win32::System::Threading::{
    CreateMutexW, EVENT_MODIFY_STATE, OpenEventW, SYNCHRONIZATION_ACCESS_RIGHTS, SetEvent,
    WaitForSingleObject,
};
use windows::Win32::UI::WindowsAndMessaging::{GA_ROOT, GetAncestor};
use windows::core::PCWSTR;

use super::inject::Hooks;
use super::protocol::{self, GraphicsOffsets, HookInfo, ShtexData};
use super::window::TargetWindow;
use crate::error::CaptureError;

/// How long to wait for the injected hook to publish its control block.
const HOOK_LOAD_TIMEOUT: Duration = Duration::from_secs(8);
/// How long to wait for the game to present a frame after `hook_init`.
const HOOK_READY_TIMEOUT: Duration = Duration::from_secs(20);

/// Owns every kernel and D3D object for one hooked game.
pub struct HookSession {
    _keepalive: OwnedHandle,
    hook_exit: OwnedHandle,
    hook_stop: OwnedHandle,
    info_map: MappedView<HookInfo>,
    _data_map: MappedView<ShtexData>,
    _device: ID3D11Device,
    context: ID3D11DeviceContext,
    shared: ID3D11Texture2D,
    staging: ID3D11Texture2D,
    width: u32,
    height: u32,
    gst_format: &'static str,
}

impl HookSession {
    /// Injects the hook (unless one is already present) and runs the
    /// handshake to first frame.
    pub fn start(
        hooks: &Hooks,
        target: &TargetWindow,
        offsets: &GraphicsOffsets,
        frame_interval_ns: u64,
    ) -> Result<Self, CaptureError> {
        let pid = target.pid;
        let keepalive = create_mutex(&suffixed(protocol::MUTEX_KEEPALIVE, pid))?;

        // If the game was hooked before, signalling restart is enough; a
        // fresh target needs the hook injected.
        if let Ok(restart) = open_event(&suffixed(protocol::EVENT_CAPTURE_RESTART, pid)) {
            info!("existing hook found for pid {pid}, signalling restart");
            // SAFETY: a valid, owned event handle.
            unsafe { SetEvent(restart.0) }.ok();
        } else {
            hooks.inject(target.is_64bit, target.thread_id)?;
        }

        // The hook creates its control block in DllMain; wait for it.
        let info_map = wait_for_hook_info(pid)?;
        // Publish the Present offsets and options the hook reads on init.
        {
            // SAFETY: the view is mapped and sized for one HookInfo.
            let info = unsafe { &mut *info_map.ptr };
            info.offsets = *offsets;
            info.capture_overlay = 0;
            info.force_shmem = 0;
            info.allow_srgb_alias = 1;
            info.frame_interval = frame_interval_ns;
        }

        let hook_init = open_event(&suffixed(protocol::EVENT_HOOK_INIT, pid))?;
        let hook_ready = open_event(&suffixed(protocol::EVENT_HOOK_READY, pid))?;
        let hook_stop = open_event(&suffixed(protocol::EVENT_CAPTURE_STOP, pid))?;
        let hook_exit = open_event(&suffixed(protocol::EVENT_HOOK_EXIT, pid))?;

        // Release the hook's capture loop; it hooks Present and, on the next
        // presented frame, sets up the shared texture and signals ready.
        // SAFETY: valid owned event handle.
        unsafe { SetEvent(hook_init.0) }
            .map_err(|e| CaptureError::GameCapture(format!("could not signal hook init: {e}")))?;

        if !wait_signalled(hook_ready.0, HOOK_READY_TIMEOUT)? {
            return Err(CaptureError::GameCapture(
                "the game did not present a frame to capture (is it running in the foreground?)"
                    .to_owned(),
            ));
        }

        // SAFETY: the info view is valid and the hook has finished writing it.
        let info = unsafe { *info_map.ptr };
        if info.capture_type != protocol::CAPTURE_TYPE_TEXTURE {
            return Err(CaptureError::GameCapture(
                "the game fell back to shared memory capture, which is not supported yet"
                    .to_owned(),
            ));
        }
        let gst_format = gst_format_for(info.format)?;

        let data_map = open_texture_data_map(&info, target.hwnd)?;
        // SAFETY: the data view is mapped and sized for one ShtexData.
        let tex_handle = unsafe { (*data_map.ptr).tex_handle };

        let (device, context) = create_device()?;
        let shared = open_shared_texture(&device, tex_handle)?;
        let staging = create_staging(&device, &shared)?;

        info!(
            "game capture ready: {}x{} {} (pid {pid})",
            info.cx, info.cy, gst_format
        );
        Ok(Self {
            _keepalive: keepalive,
            hook_exit,
            hook_stop,
            info_map,
            _data_map: data_map,
            _device: device,
            context,
            shared,
            staging,
            width: info.cx,
            height: info.cy,
            gst_format,
        })
    }

    pub fn width(&self) -> u32 {
        self.width
    }

    pub fn height(&self) -> u32 {
        self.height
    }

    pub fn format(&self) -> &'static str {
        self.gst_format
    }

    /// True while the hook is still delivering frames.
    pub fn alive(&self) -> bool {
        // SAFETY: owned event handles; a zero timeout is a state poll.
        let exited = unsafe { WaitForSingleObject(self.hook_exit.0, 0) } == WAIT_OBJECT_0;
        let stopped = unsafe { WaitForSingleObject(self.hook_stop.0, 0) } == WAIT_OBJECT_0;
        !exited && !stopped
    }

    /// Copies the latest presented frame out of the shared texture as tightly
    /// packed rows (`width * 4` bytes each). An error is fatal (the texture
    /// was lost, usually because the game resized or closed).
    pub fn read_frame(&mut self) -> Result<Vec<u8>, CaptureError> {
        // A resize republishes the control block with new dimensions, which
        // our fixed staging texture can no longer hold.
        // SAFETY: the info view stays mapped for the session's lifetime.
        let info = unsafe { *self.info_map.ptr };
        if info.cx != self.width || info.cy != self.height {
            return Err(CaptureError::GameCapture(
                "the capture size changed".to_owned(),
            ));
        }

        // SAFETY: both textures live on `context`'s device; CopyResource of
        // matching descriptions is valid, and the map is released before the
        // borrowed `mapped` pointer is used.
        unsafe {
            self.context.CopyResource(&self.staging, &self.shared);
            let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
            self.context
                .Map(&self.staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped))
                .map_err(|e| CaptureError::GameCapture(format!("could not map the frame: {e}")))?;

            let row_bytes = (self.width * 4) as usize;
            let src_pitch = mapped.RowPitch as usize;
            let mut data = vec![0u8; row_bytes * self.height as usize];
            for row in 0..self.height as usize {
                let src = (mapped.pData as *const u8).add(row * src_pitch);
                let dst = data.as_mut_ptr().add(row * row_bytes);
                std::ptr::copy_nonoverlapping(src, dst, row_bytes);
            }
            self.context.Unmap(&self.staging, 0);
            Ok(data)
        }
    }
}

/// A `HANDLE` closed on drop.
struct OwnedHandle(HANDLE);

impl Drop for OwnedHandle {
    fn drop(&mut self) {
        if !self.0.is_invalid() {
            // SAFETY: we own the handle and only close it once.
            unsafe { CloseHandle(self.0) }.ok();
        }
    }
}

/// A mapped view of a file mapping, unmapped on drop.
struct MappedView<T> {
    ptr: *mut T,
    handle: HANDLE,
    base: *mut c_void,
}

impl<T> Drop for MappedView<T> {
    fn drop(&mut self) {
        // SAFETY: `base` came from MapViewOfFile and `handle` from
        // OpenFileMapping; both are unmapped and closed exactly once.
        unsafe {
            let _ = UnmapViewOfFile(windows::Win32::System::Memory::MEMORY_MAPPED_VIEW_ADDRESS {
                Value: self.base,
            });
            let _ = CloseHandle(self.handle);
        }
    }
}

// The session confines all raw pointers to the calling thread.
unsafe impl<T> Send for MappedView<T> {}

fn suffixed(name: &str, pid: u32) -> String {
    format!("{name}{pid}")
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

fn create_mutex(name: &str) -> Result<OwnedHandle, CaptureError> {
    let w = wide(name);
    // SAFETY: `w` is a valid null terminated wide string.
    let handle = unsafe { CreateMutexW(None, false, PCWSTR(w.as_ptr())) }
        .map_err(|e| CaptureError::GameCapture(format!("could not create {name}: {e}")))?;
    Ok(OwnedHandle(handle))
}

fn open_event(name: &str) -> Result<OwnedHandle, CaptureError> {
    let w = wide(name);
    // SYNCHRONIZE (0x0010_0000) lets us wait on the event; EVENT_MODIFY_STATE
    // lets us signal it. There is no named SYNCHRONIZE of this access type.
    let access = EVENT_MODIFY_STATE | SYNCHRONIZATION_ACCESS_RIGHTS(0x0010_0000);
    // SAFETY: `w` is a valid null terminated wide string.
    let handle = unsafe { OpenEventW(access, false, PCWSTR(w.as_ptr())) }
        .map_err(|e| CaptureError::GameCapture(format!("could not open event {name}: {e}")))?;
    Ok(OwnedHandle(handle))
}

fn wait_signalled(handle: HANDLE, timeout: Duration) -> Result<bool, CaptureError> {
    let deadline = Instant::now() + timeout;
    loop {
        // SAFETY: valid handle; short poll so the caller stays responsive.
        let state = unsafe { WaitForSingleObject(handle, 100) };
        if state == WAIT_OBJECT_0 {
            return Ok(true);
        }
        if Instant::now() >= deadline {
            return Ok(false);
        }
    }
}

fn wait_for_hook_info(pid: u32) -> Result<MappedView<HookInfo>, CaptureError> {
    let name = suffixed(protocol::SHMEM_HOOK_INFO, pid);
    let deadline = Instant::now() + HOOK_LOAD_TIMEOUT;
    loop {
        if let Ok(view) = open_mapping::<HookInfo>(&name) {
            return Ok(view);
        }
        if Instant::now() >= deadline {
            return Err(CaptureError::GameCapture(
                "the capture hook did not load (the game may block injection)".to_owned(),
            ));
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn open_texture_data_map(
    info: &HookInfo,
    hwnd: HWND,
) -> Result<MappedView<ShtexData>, CaptureError> {
    // The hook names the map after the root ancestor of the hooked window.
    // SAFETY: GetAncestor on a window handle is always safe.
    let root = unsafe { GetAncestor(hwnd, GA_ROOT) };
    let candidates = [root.0 as u64, info.window as u64, hwnd.0 as u64];
    for window in candidates {
        let name = protocol::texture_map_name(window, info.map_id);
        if let Ok(view) = open_mapping::<ShtexData>(&name) {
            return Ok(view);
        }
    }
    Err(CaptureError::GameCapture(
        "could not open the shared texture data".to_owned(),
    ))
}

fn open_mapping<T>(name: &str) -> Result<MappedView<T>, CaptureError> {
    let w = wide(name);
    // SAFETY: `w` is a valid null terminated wide string.
    let handle = unsafe { OpenFileMappingW(FILE_MAP_ALL_ACCESS.0, false, PCWSTR(w.as_ptr())) }
        .map_err(|e| CaptureError::GameCapture(format!("could not open mapping {name}: {e}")))?;
    // SAFETY: the mapping handle is valid; we map the whole object.
    let base = unsafe { MapViewOfFile(handle, FILE_MAP_ALL_ACCESS, 0, 0, 0) };
    if base.Value.is_null() {
        // SAFETY: closing the just opened handle.
        unsafe { CloseHandle(handle) }.ok();
        return Err(CaptureError::GameCapture(format!("could not map {name}")));
    }
    Ok(MappedView {
        ptr: base.Value as *mut T,
        handle,
        base: base.Value,
    })
}

fn create_device() -> Result<(ID3D11Device, ID3D11DeviceContext), CaptureError> {
    let mut device = None;
    let mut context = None;
    let levels = [D3D_FEATURE_LEVEL_11_0];
    // SAFETY: out params are owned Options filled by the driver.
    unsafe {
        D3D11CreateDevice(
            None,
            D3D_DRIVER_TYPE_HARDWARE,
            windows::Win32::Foundation::HMODULE::default(),
            D3D11_CREATE_DEVICE_BGRA_SUPPORT,
            Some(&levels),
            D3D11_SDK_VERSION,
            Some(&mut device),
            None,
            Some(&mut context),
        )
    }
    .map_err(|e| CaptureError::GameCapture(format!("could not create a D3D11 device: {e}")))?;
    match (device, context) {
        (Some(device), Some(context)) => Ok((device, context)),
        _ => Err(CaptureError::GameCapture(
            "the D3D11 device was not created".to_owned(),
        )),
    }
}

fn open_shared_texture(
    device: &ID3D11Device,
    handle: u32,
) -> Result<ID3D11Texture2D, CaptureError> {
    let shared = HANDLE(handle as usize as *mut c_void);
    let mut texture: Option<ID3D11Texture2D> = None;
    // SAFETY: the handle names a texture shared by the hook on the same
    // adapter; the interface is written into `texture` and released by RAII.
    unsafe { device.OpenSharedResource::<ID3D11Texture2D>(shared, &mut texture) }.map_err(|e| {
        CaptureError::GameCapture(format!("could not open the shared texture: {e}"))
    })?;
    texture.ok_or_else(|| CaptureError::GameCapture("the shared texture was not opened".to_owned()))
}

fn create_staging(
    device: &ID3D11Device,
    shared: &ID3D11Texture2D,
) -> Result<ID3D11Texture2D, CaptureError> {
    let mut desc = D3D11_TEXTURE2D_DESC::default();
    // SAFETY: valid texture; GetDesc only writes the out param.
    unsafe { shared.GetDesc(&mut desc) };
    desc.Usage = D3D11_USAGE_STAGING;
    desc.BindFlags = 0;
    desc.CPUAccessFlags = D3D11_CPU_ACCESS_READ.0 as u32;
    desc.MiscFlags = 0;
    desc.MipLevels = 1;
    desc.ArraySize = 1;
    let mut staging = None;
    // SAFETY: a valid description; the texture is returned in the out param.
    unsafe { device.CreateTexture2D(&desc, None, Some(&mut staging)) }.map_err(|e| {
        CaptureError::GameCapture(format!("could not create the staging texture: {e}"))
    })?;
    staging
        .ok_or_else(|| CaptureError::GameCapture("the staging texture was not created".to_owned()))
}

/// Maps the game's backbuffer format to a GStreamer raw format. The hook
/// keeps the byte layout of typeless and sRGB aliases identical, so only the
/// channel order matters here.
fn gst_format_for(dxgi_format: u32) -> Result<&'static str, CaptureError> {
    match dxgi_format {
        // B8G8R8A8: TYPELESS (90), UNORM (87), UNORM_SRGB (91).
        87 | 90 | 91 => Ok("BGRA"),
        // R8G8B8A8: TYPELESS (27), UNORM (28), UNORM_SRGB (29).
        27..=29 => Ok("RGBA"),
        other => Err(CaptureError::GameCapture(format!(
            "the game uses an unsupported backbuffer format ({other})"
        ))),
    }
}
