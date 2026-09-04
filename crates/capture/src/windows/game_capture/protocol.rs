//! The shared memory protocol of the OBS Studio capture hook, ported from
//! `shared/obs-hook-config/graphics-hook-info.h` at tag 32.2.2 (hook version
//! 1.8.8). The struct layouts are a frozen ABI on the OBS side; the size
//! assertions below are the compile time proof that this port matches.

/// Kernel object name prefixes. Every name is suffixed with the target
/// process id (the texture data map additionally embeds the window handle
/// and map id, see [`texture_map_name`]).
pub const EVENT_CAPTURE_RESTART: &str = "CaptureHook_Restart";
pub const EVENT_CAPTURE_STOP: &str = "CaptureHook_Stop";
pub const EVENT_HOOK_READY: &str = "CaptureHook_HookReady";
pub const EVENT_HOOK_EXIT: &str = "CaptureHook_Exit";
pub const EVENT_HOOK_INIT: &str = "CaptureHook_Initialize";
pub const MUTEX_KEEPALIVE: &str = "CaptureHook_KeepAlive";
pub const SHMEM_HOOK_INFO: &str = "CaptureHook_HookInfo";
pub const SHMEM_TEXTURE: &str = "CaptureHook_Texture";

pub const CAPTURE_TYPE_TEXTURE: u32 = 1;

/// `CaptureHook_Texture_{window}_{map_id}`; the window is the value the hook
/// wrote into `hook_info.window`, as a decimal `u64`.
pub fn texture_map_name(window: u64, map_id: u32) -> String {
    format!("{SHMEM_TEXTURE}_{window}_{map_id}")
}

#[repr(C)]
#[derive(Debug, Default, Clone, Copy)]
pub struct D3d8Offsets {
    pub present: u32,
}

#[repr(C)]
#[derive(Debug, Default, Clone, Copy)]
pub struct D3d9Offsets {
    pub present: u32,
    pub present_ex: u32,
    pub present_swap: u32,
    pub d3d9_clsoff: u32,
    pub is_d3d9ex_clsoff: u32,
}

#[repr(C)]
#[derive(Debug, Default, Clone, Copy)]
pub struct D3d12Offsets {
    pub execute_command_lists: u32,
}

#[repr(C)]
#[derive(Debug, Default, Clone, Copy)]
pub struct DxgiOffsets {
    pub present: u32,
    pub resize: u32,
    pub present1: u32,
}

#[repr(C)]
#[derive(Debug, Default, Clone, Copy)]
pub struct DxgiOffsets2 {
    pub release: u32,
}

#[repr(C)]
#[derive(Debug, Default, Clone, Copy)]
pub struct DdrawOffsets {
    pub surface_create: u32,
    pub surface_restore: u32,
    pub surface_release: u32,
    pub surface_unlock: u32,
    pub surface_blt: u32,
    pub surface_flip: u32,
    pub surface_set_palette: u32,
    pub palette_set_entries: u32,
}

#[repr(C)]
#[derive(Debug, Default, Clone, Copy)]
pub struct GraphicsOffsets {
    pub d3d8: D3d8Offsets,
    pub d3d9: D3d9Offsets,
    pub dxgi: DxgiOffsets,
    pub ddraw: DdrawOffsets,
    pub dxgi2: DxgiOffsets2,
    pub d3d12: D3d12Offsets,
}

/// The 648 byte control block the hook creates as `CaptureHook_HookInfo{pid}`.
/// Booleans are `u8` because the memory is written by foreign code. The C
/// original is `#pragma pack(push, 8)`, which matches Rust's natural layout
/// for these field types.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct HookInfo {
    pub hook_ver_major: u32,
    pub hook_ver_minor: u32,

    pub capture_type: u32,
    pub window: u32,
    pub format: u32,
    pub cx: u32,
    pub cy: u32,
    pub unused_base_cx: u32,
    pub unused_base_cy: u32,
    pub pitch: u32,
    pub map_id: u32,
    pub map_size: u32,
    pub flip: u8,
    pub padding1: [u8; 7],

    pub frame_interval: u64,
    pub unused_use_scale: u8,
    pub force_shmem: u8,
    pub capture_overlay: u8,
    pub allow_srgb_alias: u8,

    pub offsets: GraphicsOffsets,

    pub reserved: [u32; 126],
}

/// First bytes of the texture data map in shared texture mode: the D3D shared
/// handle of the texture the hook copies every presented frame into. (Shared
/// memory mode, used when the GPU cannot share textures, is not supported.)
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct ShtexData {
    pub tex_handle: u32,
}

/// Parses the ini style stdout of `get-graphics-offsets*.exe`.
pub fn parse_offsets(text: &str) -> GraphicsOffsets {
    let mut offsets = GraphicsOffsets::default();
    let mut section = String::new();
    for line in text.lines() {
        let line = line.trim();
        if let Some(name) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            section = name.to_owned();
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let value = parse_number(value.trim());
        match (section.as_str(), key.trim()) {
            ("d3d8", "present") => offsets.d3d8.present = value,
            ("d3d9", "present") => offsets.d3d9.present = value,
            ("d3d9", "present_ex") => offsets.d3d9.present_ex = value,
            ("d3d9", "present_swap") => offsets.d3d9.present_swap = value,
            ("d3d9", "d3d9_clsoff") => offsets.d3d9.d3d9_clsoff = value,
            ("d3d9", "is_d3d9ex_clsoff") => offsets.d3d9.is_d3d9ex_clsoff = value,
            ("dxgi", "present") => offsets.dxgi.present = value,
            ("dxgi", "present1") => offsets.dxgi.present1 = value,
            ("dxgi", "resize") => offsets.dxgi.resize = value,
            ("dxgi", "release") => offsets.dxgi2.release = value,
            _ => {}
        }
    }
    offsets
}

fn parse_number(value: &str) -> u32 {
    if let Some(hex) = value.strip_prefix("0x") {
        u32::from_str_radix(hex, 16).unwrap_or(0)
    } else {
        value.parse().unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::mem::{offset_of, size_of};

    #[test]
    fn hook_info_matches_the_c_abi() {
        assert_eq!(size_of::<HookInfo>(), 648);
        assert_eq!(offset_of!(HookInfo, capture_type), 8);
        assert_eq!(offset_of!(HookInfo, map_id), 40);
        assert_eq!(offset_of!(HookInfo, flip), 48);
        assert_eq!(offset_of!(HookInfo, frame_interval), 56);
        assert_eq!(offset_of!(HookInfo, force_shmem), 65);
        assert_eq!(offset_of!(HookInfo, offsets), 68);
        assert_eq!(offset_of!(HookInfo, reserved), 144);
        assert_eq!(size_of::<GraphicsOffsets>(), 76);
    }

    #[test]
    fn parses_offsets_output() {
        let text = "[d3d8]\npresent=0x0\n[d3d9]\npresent=0x55a90\npresent_ex=0x56000\n\
                    present_swap=0x2e0\nd3d9_clsoff=0x0\nis_d3d9ex_clsoff=0x0\n\
                    [dxgi]\npresent=0x2b00\npresent1=0x2c40\nresize=0x1a20\nrelease=0xb50\n";
        let offsets = parse_offsets(text);
        assert_eq!(offsets.d3d9.present, 0x55a90);
        assert_eq!(offsets.dxgi.present, 0x2b00);
        assert_eq!(offsets.dxgi.present1, 0x2c40);
        assert_eq!(offsets.dxgi.resize, 0x1a20);
        assert_eq!(offsets.dxgi2.release, 0xb50);
        assert_eq!(offsets.d3d8.present, 0);
    }

    #[test]
    fn texture_map_name_embeds_window_and_map_id() {
        assert_eq!(
            texture_map_name(0x000306ac, 42),
            "CaptureHook_Texture_198316_42"
        );
    }
}
