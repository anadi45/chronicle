//! Windows active-window screenshot provider.
//!
//! This is a CPU-backed fallback for environments where Windows Graphics
//! Capture cannot initialize. Bytes remain in memory and are returned as a
//! bounded PNG payload for the transient screenshot pipeline.

#[cfg(windows)]
use windows::Win32::Foundation::{HWND, RECT};
#[cfg(windows)]
use windows::Win32::Graphics::Gdi::*;
#[cfg(windows)]
use windows::Win32::UI::WindowsAndMessaging::GetClientRect;

#[cfg(windows)]
pub fn capture_window_png(window_handle: isize) -> Result<Vec<u8>, String> {
    let hwnd = HWND(window_handle as *mut core::ffi::c_void);
    let mut rect = RECT::default();
    unsafe {
        GetClientRect(hwnd, &mut rect).map_err(|error| error.to_string())?;
    }
    let width = (rect.right - rect.left) as i32;
    let height = (rect.bottom - rect.top) as i32;
    if width <= 0 || height <= 0 || (width as i64 * height as i64) > 16_000_000 {
        return Err("window dimensions are invalid or exceed screenshot limit".into());
    }
    let source = unsafe { GetDC(Some(hwnd)) };
    if source.is_invalid() {
        return Err("GetDC failed".into());
    }
    let memory = unsafe { CreateCompatibleDC(Some(source)) };
    if memory.is_invalid() {
        unsafe {
            ReleaseDC(Some(hwnd), source);
        }
        return Err("CreateCompatibleDC failed".into());
    }
    let bitmap = unsafe { CreateCompatibleBitmap(source, width, height) };
    if bitmap.is_invalid() {
        unsafe {
            let _ = DeleteDC(memory);
            let _ = ReleaseDC(Some(hwnd), source);
        }
        return Err("CreateCompatibleBitmap failed".into());
    }
    let previous = unsafe { SelectObject(memory, bitmap.into()) };
    let copied =
        unsafe { BitBlt(memory, 0, 0, width, height, Some(source), 0, 0, SRCCOPY).is_ok() };
    let mut pixels = vec![0u8; width as usize * height as usize * 4];
    let mut info = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: core::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: width,
            biHeight: -height,
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            ..Default::default()
        },
        ..Default::default()
    };
    let read = if copied {
        unsafe {
            GetDIBits(
                memory,
                bitmap,
                0,
                height as u32,
                Some(pixels.as_mut_ptr().cast()),
                &mut info,
                DIB_RGB_COLORS,
            )
        }
    } else {
        0
    };
    unsafe {
        SelectObject(memory, previous);
        let _ = DeleteObject(bitmap.into());
        let _ = DeleteDC(memory);
        let _ = ReleaseDC(Some(hwnd), source);
    }
    if read == 0 {
        return Err("GetDIBits failed".into());
    }
    for pixel in pixels.chunks_exact_mut(4) {
        pixel.swap(0, 2);
        pixel[3] = 255;
    }
    encode_png_rgba(width as u32, height as u32, &pixels)
}

#[cfg(not(windows))]
pub fn capture_window_png(_window_handle: isize) -> Result<Vec<u8>, String> {
    Err("Windows screenshot provider is unavailable on this platform".into())
}

pub(crate) fn encode_png_rgba(width: u32, height: u32, rgba: &[u8]) -> Result<Vec<u8>, String> {
    if rgba.len() != width as usize * height as usize * 4 {
        return Err("pixel buffer dimensions do not match".into());
    }
    let mut scanlines = Vec::with_capacity((rgba.len() + height as usize) + 6);
    for row in rgba.chunks_exact(width as usize * 4) {
        scanlines.push(0);
        scanlines.extend_from_slice(row);
    }
    let mut compressed = Vec::with_capacity(scanlines.len() + 6 + scanlines.len() / 65535 * 5);
    compressed.extend_from_slice(&[0x78, 0x01]);
    for (index, chunk) in scanlines.chunks(65_535).enumerate() {
        let final_block = index == (scanlines.len() - 1) / 65_535;
        compressed.push(if final_block { 1 } else { 0 });
        let len = chunk.len() as u16;
        compressed.extend_from_slice(&len.to_le_bytes());
        compressed.extend_from_slice(&(!len).to_le_bytes());
        compressed.extend_from_slice(chunk);
    }
    compressed.extend_from_slice(&adler32(&scanlines).to_be_bytes());
    let mut png = vec![137, 80, 78, 71, 13, 10, 26, 10];
    let mut header = Vec::with_capacity(13);
    header.extend_from_slice(&width.to_be_bytes());
    header.extend_from_slice(&height.to_be_bytes());
    header.extend_from_slice(&[8, 6, 0, 0, 0]);
    append_chunk(&mut png, b"IHDR", &header);
    append_chunk(&mut png, b"IDAT", &compressed);
    append_chunk(&mut png, b"IEND", &[]);
    Ok(png)
}

fn append_chunk(output: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
    output.extend_from_slice(&(data.len() as u32).to_be_bytes());
    output.extend_from_slice(kind);
    output.extend_from_slice(data);
    let mut crc_input = Vec::with_capacity(4 + data.len());
    crc_input.extend_from_slice(kind);
    crc_input.extend_from_slice(data);
    output.extend_from_slice(&crc32(&crc_input).to_be_bytes());
}
fn adler32(bytes: &[u8]) -> u32 {
    let (mut a, mut b) = (1u32, 0u32);
    for byte in bytes {
        a = (a + *byte as u32) % 65521;
        b = (b + a) % 65521;
    }
    (b << 16) | a
}
fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = 0xffff_ffffu32;
    for byte in bytes {
        crc ^= *byte as u32;
        for _ in 0..8 {
            crc = if crc & 1 != 0 {
                (crc >> 1) ^ 0xedb8_8320
            } else {
                crc >> 1
            };
        }
    }
    !crc
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn png_encoder_emits_valid_signature() {
        let png = encode_png_rgba(1, 1, &[255, 0, 0, 255]).unwrap();
        assert_eq!(&png[..8], &[137, 80, 78, 71, 13, 10, 26, 10]);
        assert!(png.windows(4).any(|chunk| chunk == b"IHDR"));
    }
}
#[cfg(all(test, windows))]
#[test]
fn invalid_window_handle_is_reported_without_panicking() {
    assert!(capture_window_png(0).is_err());
}
