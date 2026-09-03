//! Extracts the icon embedded in an executable through the shell and saves
//! it as a PNG. No icon database needed: the game ships its own art.

use std::ffi::c_void;
use std::mem::size_of;
use std::path::Path;

use windows::Win32::Graphics::Gdi::{
    BI_RGB, BITMAP, BITMAPINFO, BITMAPINFOHEADER, DIB_RGB_COLORS, DeleteObject, GetDC, GetDIBits,
    GetObjectW, HBITMAP, HGDIOBJ, ReleaseDC,
};
use windows::Win32::Storage::FileSystem::FILE_FLAGS_AND_ATTRIBUTES;
use windows::Win32::UI::Shell::{SHFILEINFOW, SHGFI_ICON, SHGFI_LARGEICON, SHGetFileInfoW};
use windows::Win32::UI::WindowsAndMessaging::{DestroyIcon, GetIconInfo, HICON, ICONINFO};
use windows::core::PCWSTR;

use crate::backend::IconExtractor;
use crate::error::CaptureError;

pub struct ShellIconExtractor;

fn icon_error(path: &Path, reason: impl ToString) -> CaptureError {
    CaptureError::Media {
        path: path.to_path_buf(),
        reason: reason.to_string(),
    }
}

struct Pixels {
    width: u32,
    height: u32,
    rgba: Vec<u8>,
}

/// Reads a bitmap as top down 32 bit BGRA.
unsafe fn read_bitmap(bitmap: HBITMAP) -> Option<(u32, u32, Vec<u8>)> {
    // SAFETY: caller guarantees `bitmap` is a valid GDI bitmap.
    unsafe {
        let mut info = BITMAP::default();
        let read = GetObjectW(
            HGDIOBJ(bitmap.0),
            size_of::<BITMAP>() as i32,
            Some(&mut info as *mut BITMAP as *mut c_void),
        );
        if read == 0 || info.bmWidth <= 0 || info.bmHeight <= 0 {
            return None;
        }
        let (width, height) = (info.bmWidth as u32, info.bmHeight as u32);
        let mut header = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: info.bmWidth,
                biHeight: -info.bmHeight,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut data = vec![0u8; (width * height * 4) as usize];
        let hdc = GetDC(None);
        let lines = GetDIBits(
            hdc,
            bitmap,
            0,
            height,
            Some(data.as_mut_ptr() as *mut c_void),
            &mut header,
            DIB_RGB_COLORS,
        );
        ReleaseDC(None, hdc);
        (lines > 0).then_some((width, height, data))
    }
}

/// Converts an icon to RGBA pixels. Icons without an alpha channel get
/// their transparency from the mask bitmap.
fn icon_pixels(icon: HICON, path: &Path) -> Result<Pixels, CaptureError> {
    // SAFETY: `icon` is a valid icon handle owned by the caller; GDI objects
    // returned by GetIconInfo are deleted before returning.
    unsafe {
        let mut info = ICONINFO::default();
        GetIconInfo(icon, &mut info).map_err(|e| icon_error(path, e))?;
        let color = read_bitmap(info.hbmColor);
        let mask = read_bitmap(info.hbmMask);
        let _ = DeleteObject(HGDIOBJ(info.hbmColor.0));
        let _ = DeleteObject(HGDIOBJ(info.hbmMask.0));

        let (width, height, mut bgra) =
            color.ok_or_else(|| icon_error(path, "icon has no color bitmap"))?;
        let has_alpha = bgra.as_chunks::<4>().0.iter().any(|p| p[3] != 0);
        if !has_alpha
            && let Some((mw, mh, mask)) = mask
            && mw == width
            && mh >= height
        {
            let (mask_pixels, _) = mask.as_chunks::<4>();
            for (pixel, m) in bgra.as_chunks_mut::<4>().0.iter_mut().zip(mask_pixels) {
                pixel[3] = if m[0] == 0 { 255 } else { 0 };
            }
        } else if !has_alpha {
            for pixel in bgra.as_chunks_mut::<4>().0.iter_mut() {
                pixel[3] = 255;
            }
        }
        for pixel in bgra.as_chunks_mut::<4>().0.iter_mut() {
            pixel.swap(0, 2);
        }
        Ok(Pixels {
            width,
            height,
            rgba: bgra,
        })
    }
}

impl IconExtractor for ShellIconExtractor {
    fn extract_png(&self, exe: &Path, output: &Path) -> Result<(), CaptureError> {
        let wide: Vec<u16> = exe
            .as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();
        let mut info = SHFILEINFOW::default();
        // SAFETY: `wide` is null terminated and `info` is a valid out struct.
        let ok = unsafe {
            SHGetFileInfoW(
                PCWSTR(wide.as_ptr()),
                FILE_FLAGS_AND_ATTRIBUTES(0),
                Some(&mut info),
                size_of::<SHFILEINFOW>() as u32,
                SHGFI_ICON | SHGFI_LARGEICON,
            )
        };
        if ok == 0 || info.hIcon.0.is_null() {
            return Err(icon_error(exe, "no icon"));
        }
        let pixels = icon_pixels(info.hIcon, exe);
        // SAFETY: the icon was returned by SHGetFileInfoW and is ours to free.
        unsafe {
            let _ = DestroyIcon(info.hIcon);
        }
        let pixels = pixels?;

        if let Some(dir) = output.parent() {
            std::fs::create_dir_all(dir).map_err(|e| icon_error(exe, e))?;
        }
        let file = std::fs::File::create(output).map_err(|e| icon_error(exe, e))?;
        let mut encoder =
            png::Encoder::new(std::io::BufWriter::new(file), pixels.width, pixels.height);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header().map_err(|e| icon_error(exe, e))?;
        writer
            .write_image_data(&pixels.rgba)
            .map_err(|e| icon_error(exe, e))?;
        Ok(())
    }
}

use std::os::windows::ffi::OsStrExt;
