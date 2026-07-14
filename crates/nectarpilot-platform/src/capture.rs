//! Bounded capture of the adopted Roblox client area.
//!
//! This module never captures the desktop or another process. On Windows it
//! revalidates the exact PID/HWND pair before asking GDI for the client DC, and
//! rejects a resized, moved, or minimized window until the session owner has
//! refreshed calibration.

use chrono::{DateTime, Utc};
use image::RgbaImage;
use nectarpilot_contracts::NormalizedRegion;
use thiserror::Error;

use crate::session::{RobloxSession, SessionTarget};

/// A hard upper bound prevents an unexpected DPI or malformed window from
/// allocating unbounded image buffers.
pub const MAX_CAPTURE_PIXELS: u64 = 16_777_216;

#[derive(Debug, Error)]
pub enum CaptureError {
    #[error("the client capture has zero dimensions")]
    EmptyFrame,
    #[error("the client frame exceeds the {MAX_CAPTURE_PIXELS}-pixel safety limit")]
    FrameTooLarge,
    #[error("the requested normalized region is invalid")]
    InvalidRegion,
    #[error("the adopted Roblox session changed geometry; refresh and recalibrate before capture")]
    GeometryChanged,
    #[error("the adopted Roblox client is minimized")]
    Minimized,
    #[error("capture target does not match the adopted Roblox session")]
    TargetMismatch,
    #[error("session query failed: {0}")]
    Session(String),
    #[error("Windows client capture failed: {0}")]
    Backend(String),
    #[error("Windows client capture is unavailable on this platform")]
    UnsupportedPlatform,
}

/// An RGBA frame taken only from the verified Roblox client rectangle.
#[derive(Clone, Debug)]
pub struct ClientFrame {
    pub target: SessionTarget,
    pub geometry_revision: u64,
    pub captured_at: DateTime<Utc>,
    image: RgbaImage,
}

impl ClientFrame {
    pub fn new(
        target: SessionTarget,
        geometry_revision: u64,
        captured_at: DateTime<Utc>,
        image: RgbaImage,
    ) -> Result<Self, CaptureError> {
        validate_dimensions(image.width(), image.height())?;
        Ok(Self {
            target,
            geometry_revision,
            captured_at,
            image,
        })
    }

    #[must_use]
    pub const fn image(&self) -> &RgbaImage {
        &self.image
    }

    /// Extracts an exact, client-relative crop. The source full frame remains
    /// in memory only; diagnostic storage may persist cropped regions through
    /// `EvidenceStore` but never this frame by default.
    pub fn crop(&self, region: NormalizedRegion) -> Result<NormalizedCrop, CaptureError> {
        let pixel_region = normalized_to_pixels(region, self.image.width(), self.image.height())?;
        Ok(NormalizedCrop {
            region,
            image: image::imageops::crop_imm(
                &self.image,
                pixel_region.x,
                pixel_region.y,
                pixel_region.width,
                pixel_region.height,
            )
            .to_image(),
        })
    }
}

/// A materialized crop with its source-relative location retained for evidence
/// and normalization of template-match coordinates.
#[derive(Clone, Debug)]
pub struct NormalizedCrop {
    pub region: NormalizedRegion,
    pub image: RgbaImage,
}

/// Pixel coordinates inside a client frame. This is deliberately separate from
/// desktop coordinates so a detector cannot accidentally target another window.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PixelRegion {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

/// Converts an approved normalized region to a non-empty, in-bounds crop.
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    reason = "NormalizedRegion is deliberately f32; bounded client frames are capped at 2^24 pixels, which f32 represents exactly"
)]
pub fn normalized_to_pixels(
    region: NormalizedRegion,
    frame_width: u32,
    frame_height: u32,
) -> Result<PixelRegion, CaptureError> {
    if !region.is_valid() || frame_width == 0 || frame_height == 0 {
        return Err(CaptureError::InvalidRegion);
    }
    validate_dimensions(frame_width, frame_height)?;
    let left = (region.x * frame_width as f32).floor() as u32;
    let top = (region.y * frame_height as f32).floor() as u32;
    let right = ((region.x + region.width) * frame_width as f32)
        .ceil()
        .min(frame_width as f32) as u32;
    let bottom = ((region.y + region.height) * frame_height as f32)
        .ceil()
        .min(frame_height as f32) as u32;
    let width = right.checked_sub(left).ok_or(CaptureError::InvalidRegion)?;
    let height = bottom.checked_sub(top).ok_or(CaptureError::InvalidRegion)?;
    if width == 0 || height == 0 {
        return Err(CaptureError::InvalidRegion);
    }
    Ok(PixelRegion {
        x: left,
        y: top,
        width,
        height,
    })
}

fn validate_dimensions(width: u32, height: u32) -> Result<(), CaptureError> {
    if width == 0 || height == 0 {
        return Err(CaptureError::EmptyFrame);
    }
    let pixels = u64::from(width)
        .checked_mul(u64::from(height))
        .ok_or(CaptureError::FrameTooLarge)?;
    if pixels > MAX_CAPTURE_PIXELS {
        return Err(CaptureError::FrameTooLarge);
    }
    Ok(())
}

/// A source that produces a bounded client-only screenshot for an already
/// adopted `RobloxSession`.
pub trait ClientCapture: Send + Sync {
    fn capture(&self, session: &RobloxSession) -> Result<ClientFrame, CaptureError>;
}

/// Native Windows client-area capture. It intentionally has no desktop capture
/// API: callers can only supply the session already attached to Roblox.
#[derive(Clone, Copy, Debug, Default)]
pub struct WindowsClientCapture;

#[cfg(windows)]
impl ClientCapture for WindowsClientCapture {
    fn capture(&self, session: &RobloxSession) -> Result<ClientFrame, CaptureError> {
        use crate::session::SessionProbe;
        use crate::windows_backend::WindowsSessionProbe;

        let probe = WindowsSessionProbe;
        let snapshot = probe
            .snapshot(session.target())
            .map_err(|error| CaptureError::Session(error.to_string()))?;
        if snapshot.geometry != session.geometry() {
            return Err(CaptureError::GeometryChanged);
        }
        if snapshot.geometry.minimized {
            return Err(CaptureError::Minimized);
        }
        validate_dimensions(
            snapshot.geometry.client.width,
            snapshot.geometry.client.height,
        )?;
        let image = capture_client_area(
            session.target().window.get(),
            snapshot.geometry.client.width,
            snapshot.geometry.client.height,
        )?;
        ClientFrame::new(
            session.target(),
            session.geometry_revision(),
            Utc::now(),
            image,
        )
    }
}

#[cfg(not(windows))]
impl ClientCapture for WindowsClientCapture {
    fn capture(&self, _session: &RobloxSession) -> Result<ClientFrame, CaptureError> {
        Err(CaptureError::UnsupportedPlatform)
    }
}

#[cfg(windows)]
fn capture_client_area(
    raw_window: u64,
    width: u32,
    height: u32,
) -> Result<RgbaImage, CaptureError> {
    use std::ffi::c_void;
    use std::mem::size_of;
    use windows::Win32::Foundation::HWND;
    use windows::Win32::Graphics::Gdi::{
        BI_RGB, BITMAPINFO, BITMAPINFOHEADER, BitBlt, GdiFlush, ROP_CODE, SRCCOPY,
    };

    let width_i32 = i32::try_from(width)
        .map_err(|_| CaptureError::Backend("client width does not fit Win32".to_owned()))?;
    let height_i32 = i32::try_from(height)
        .map_err(|_| CaptureError::Backend("client height does not fit Win32".to_owned()))?;
    // NectarPilot is x64-only; conversion still remains checked so a malformed
    // opaque handle cannot silently truncate on an unsupported pointer width.
    let window_bits = usize::try_from(raw_window).map_err(|_| {
        CaptureError::Backend("window handle does not fit this platform".to_owned())
    })?;
    let window = HWND(window_bits as *mut c_void);
    let source = OwnedClientDc::new(window)?;
    let memory = OwnedMemoryDc::new(source.0)?;
    let mut info = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: u32::try_from(size_of::<BITMAPINFOHEADER>()).unwrap_or(u32::MAX),
            biWidth: width_i32,
            // A negative height requests a top-down DIB, matching image crate's
            // raster ordering without a post-capture vertical flip.
            biHeight: -height_i32,
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            ..BITMAPINFOHEADER::default()
        },
        ..BITMAPINFO::default()
    };
    let (bitmap, bits) = OwnedBitmap::new_dib(source.0, &mut info)?;
    let pixels = usize::try_from(u64::from(width) * u64::from(height))
        .map_err(|_| CaptureError::FrameTooLarge)?;
    let byte_len = pixels.checked_mul(4).ok_or(CaptureError::FrameTooLarge)?;
    let mut rgba = vec![0_u8; byte_len];
    {
        let _selection = SelectedBitmap::new(memory.0, bitmap.0)?;
        // SAFETY: both DCs are valid, the selected bitmap has the exact bounded
        // client dimensions, and the source is the adopted window's client DC.
        unsafe {
            BitBlt(
                memory.0,
                0,
                0,
                width_i32,
                height_i32,
                Some(source.0),
                0,
                0,
                ROP_CODE(SRCCOPY.0),
            )
        }
        .map_err(|error| CaptureError::Backend(error.to_string()))?;
        // CreateDIBSection exposes memory owned by the bitmap. GDI can still
        // have an asynchronous write in flight after BitBlt, so synchronize
        // before touching that memory (the Win32 API explicitly requires it).
        if !unsafe { GdiFlush() }.as_bool() {
            return Err(CaptureError::Backend(
                windows::core::Error::from_win32().to_string(),
            ));
        }
        // SAFETY: `bits` was returned by CreateDIBSection for a 32-bit DIB with
        // these exact checked dimensions. The bitmap remains alive throughout
        // this scope, and each BGRA pixel is copied into Rust-owned memory.
        let bgra = unsafe { std::slice::from_raw_parts(bits.cast_const(), byte_len) };
        for (source, destination) in bgra.chunks_exact(4).zip(rgba.chunks_exact_mut(4)) {
            destination[0] = source[2];
            destination[1] = source[1];
            destination[2] = source[0];
            destination[3] = 255;
        }
    }
    RgbaImage::from_raw(width, height, rgba).ok_or_else(|| {
        CaptureError::Backend("captured buffer did not match the expected RGBA layout".to_owned())
    })
}

#[cfg(windows)]
struct OwnedClientDc(
    windows::Win32::Graphics::Gdi::HDC,
    windows::Win32::Foundation::HWND,
);

#[cfg(windows)]
impl OwnedClientDc {
    fn new(window: windows::Win32::Foundation::HWND) -> Result<Self, CaptureError> {
        // SAFETY: window is the already revalidated adopted HWND. The HDC is
        // released exactly once by this RAII wrapper.
        let dc = unsafe { windows::Win32::Graphics::Gdi::GetDC(Some(window)) };
        if dc.is_invalid() {
            Err(CaptureError::Backend(
                windows::core::Error::from_win32().to_string(),
            ))
        } else {
            Ok(Self(dc, window))
        }
    }
}

#[cfg(windows)]
impl Drop for OwnedClientDc {
    fn drop(&mut self) {
        // SAFETY: this wrapper owns the `(HWND, HDC)` pair obtained by GetDC.
        let _ = unsafe { windows::Win32::Graphics::Gdi::ReleaseDC(Some(self.1), self.0) };
    }
}

#[cfg(windows)]
struct OwnedMemoryDc(windows::Win32::Graphics::Gdi::HDC);

#[cfg(windows)]
impl OwnedMemoryDc {
    fn new(source: windows::Win32::Graphics::Gdi::HDC) -> Result<Self, CaptureError> {
        // SAFETY: source is a valid client DC; the compatible memory DC is
        // destroyed exactly once by this wrapper.
        let dc = unsafe { windows::Win32::Graphics::Gdi::CreateCompatibleDC(Some(source)) };
        if dc.is_invalid() {
            Err(CaptureError::Backend(
                windows::core::Error::from_win32().to_string(),
            ))
        } else {
            Ok(Self(dc))
        }
    }
}

#[cfg(windows)]
impl Drop for OwnedMemoryDc {
    fn drop(&mut self) {
        // SAFETY: this wrapper owns the memory DC created by CreateCompatibleDC.
        let _ = unsafe { windows::Win32::Graphics::Gdi::DeleteDC(self.0) };
    }
}

#[cfg(windows)]
struct OwnedBitmap(windows::Win32::Graphics::Gdi::HBITMAP);

#[cfg(windows)]
impl OwnedBitmap {
    fn new_dib(
        source: windows::Win32::Graphics::Gdi::HDC,
        info: &mut windows::Win32::Graphics::Gdi::BITMAPINFO,
    ) -> Result<(Self, *mut u8), CaptureError> {
        use std::ffi::c_void;
        use std::ptr;

        let mut bits: *mut c_void = ptr::null_mut();
        // SAFETY: source is valid, `info` describes a bounded 32-bit DIB, and
        // `bits` is a writable out-pointer. The returned allocation is owned by
        // the HBITMAP and stays valid until OwnedBitmap drops it.
        let bitmap = unsafe {
            windows::Win32::Graphics::Gdi::CreateDIBSection(
                Some(source),
                info,
                windows::Win32::Graphics::Gdi::DIB_RGB_COLORS,
                &raw mut bits,
                None,
                0,
            )
        }
        .map_err(|error| CaptureError::Backend(error.to_string()))?;
        if bitmap.is_invalid() {
            Err(CaptureError::Backend(
                windows::core::Error::from_win32().to_string(),
            ))
        } else if bits.is_null() {
            // A successful DIB section must provide writable bits. Delete the
            // bitmap immediately so a malformed native result cannot leak.
            let _ = unsafe {
                windows::Win32::Graphics::Gdi::DeleteObject(
                    windows::Win32::Graphics::Gdi::HGDIOBJ::from(bitmap),
                )
            };
            Err(CaptureError::Backend(
                "CreateDIBSection returned a bitmap without writable pixels".to_owned(),
            ))
        } else {
            Ok((Self(bitmap), bits.cast::<u8>()))
        }
    }
}

#[cfg(windows)]
impl Drop for OwnedBitmap {
    fn drop(&mut self) {
        // SAFETY: the selection guard restores the original object before this
        // bitmap is dropped, and this wrapper owns it exactly once.
        let _ = unsafe {
            windows::Win32::Graphics::Gdi::DeleteObject(
                windows::Win32::Graphics::Gdi::HGDIOBJ::from(self.0),
            )
        };
    }
}

#[cfg(windows)]
struct SelectedBitmap(
    windows::Win32::Graphics::Gdi::HDC,
    windows::Win32::Graphics::Gdi::HGDIOBJ,
);

#[cfg(windows)]
impl SelectedBitmap {
    fn new(
        dc: windows::Win32::Graphics::Gdi::HDC,
        bitmap: windows::Win32::Graphics::Gdi::HBITMAP,
    ) -> Result<Self, CaptureError> {
        // SAFETY: both handles are owned by the current capture operation. The
        // returned original object is restored by Drop before bitmap cleanup.
        let original = unsafe {
            windows::Win32::Graphics::Gdi::SelectObject(
                dc,
                windows::Win32::Graphics::Gdi::HGDIOBJ::from(bitmap),
            )
        };
        if original.is_invalid() {
            Err(CaptureError::Backend(
                windows::core::Error::from_win32().to_string(),
            ))
        } else {
            Ok(Self(dc, original))
        }
    }
}

#[cfg(windows)]
impl Drop for SelectedBitmap {
    fn drop(&mut self) {
        // SAFETY: this restores the original object returned by SelectObject to
        // the same memory DC before that DC and bitmap are destroyed.
        let _ = unsafe { windows::Win32::Graphics::Gdi::SelectObject(self.0, self.1) };
    }
}

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use image::{Rgba, RgbaImage};
    use nectarpilot_contracts::NormalizedRegion;

    use super::{
        CaptureError, ClientFrame, MAX_CAPTURE_PIXELS, normalized_to_pixels, validate_dimensions,
    };
    use crate::session::{ProcessId, SessionTarget, WindowHandle};

    fn target() -> SessionTarget {
        SessionTarget {
            pid: ProcessId::new(27).unwrap(),
            window: WindowHandle::new(28).unwrap(),
        }
    }

    #[test]
    fn normalized_crop_is_bounded_to_the_client_frame() {
        let mut image = RgbaImage::new(100, 80);
        image.put_pixel(20, 20, Rgba([7, 8, 9, 255]));
        let frame = ClientFrame::new(target(), 0, Utc::now(), image).unwrap();
        let crop = frame
            .crop(NormalizedRegion {
                x: 0.2,
                y: 0.25,
                width: 0.2,
                height: 0.25,
            })
            .unwrap();

        assert_eq!(crop.image.dimensions(), (20, 20));
        assert_eq!(crop.image.get_pixel(0, 0), &Rgba([7, 8, 9, 255]));
    }

    #[test]
    fn invalid_normalized_regions_are_rejected() {
        let error = normalized_to_pixels(
            NormalizedRegion {
                x: 0.9,
                y: 0.0,
                width: 0.2,
                height: 0.2,
            },
            100,
            100,
        )
        .unwrap_err();
        assert!(matches!(error, CaptureError::InvalidRegion));
    }

    #[test]
    fn pathological_capture_dimensions_are_rejected_before_allocation() {
        let width = u32::try_from(MAX_CAPTURE_PIXELS + 1).unwrap();
        let error = validate_dimensions(width, 1).unwrap_err();
        assert!(matches!(error, CaptureError::FrameTooLarge));
    }

    #[cfg(windows)]
    #[test]
    fn dib_sections_expose_bounded_writable_bgra_memory() {
        use std::mem::size_of;

        use windows::Win32::Graphics::Gdi::{
            BI_RGB, BITMAPINFO, BITMAPINFOHEADER, CreateCompatibleDC, DeleteDC,
        };

        use super::OwnedBitmap;

        // SAFETY: a memory DC has no external window ownership and is released
        // exactly once before the test returns.
        let dc = unsafe { CreateCompatibleDC(None) };
        assert!(!dc.is_invalid());
        let mut info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: u32::try_from(size_of::<BITMAPINFOHEADER>()).unwrap(),
                biWidth: 2,
                biHeight: -1,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..BITMAPINFOHEADER::default()
            },
            ..BITMAPINFO::default()
        };
        let (bitmap, bits) = OwnedBitmap::new_dib(dc, &mut info).unwrap();
        // SAFETY: the 2x1, 32-bit DIB owns exactly eight writable bytes while
        // `bitmap` remains in scope.
        unsafe {
            *bits.add(0) = 0x03;
            *bits.add(1) = 0x02;
            *bits.add(2) = 0x01;
            *bits.add(3) = 0xFF;
            assert_eq!(
                std::slice::from_raw_parts(bits, 4),
                [0x03, 0x02, 0x01, 0xFF]
            );
        }
        drop(bitmap);
        // SAFETY: this balances CreateCompatibleDC above.
        let _ = unsafe { DeleteDC(dc) };
    }
}
