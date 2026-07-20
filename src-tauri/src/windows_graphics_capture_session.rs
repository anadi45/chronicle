//! Windows Graphics Capture D3D11 session initialization.
//!
//! The session owns the GPU capture resources. CPU-backed PNG encoding uses
//! the fallback provider until the surface readback path is available.

#[cfg(windows)]
pub struct D3d11CaptureSession {
    pub frame_pool: windows::Graphics::Capture::Direct3D11CaptureFramePool,
    pub session: windows::Graphics::Capture::GraphicsCaptureSession,
}

#[cfg(windows)]
pub fn initialize(window_handle: isize) -> Result<D3d11CaptureSession, String> {
    use windows::Graphics::Capture::{Direct3D11CaptureFramePool, GraphicsCaptureItem};
    use windows::Graphics::DirectX::{DirectXPixelFormat, Direct3D11::IDirect3DDevice};
    use windows::UI::WindowId;
    use windows::core::Interface;
    use windows::Win32::Foundation::HMODULE;
    use windows::Win32::Graphics::Direct3D::{D3D_DRIVER_TYPE_HARDWARE, D3D_FEATURE_LEVEL_11_0};
    use windows::Win32::Graphics::Direct3D11::{D3D11CreateDevice, D3D11_CREATE_DEVICE_FLAG, ID3D11Device};
    use windows::Win32::Graphics::Dxgi::IDXGIDevice;
    use windows::Win32::System::WinRT::Direct3D11::CreateDirect3D11DeviceFromDXGIDevice;
    let item = GraphicsCaptureItem::TryCreateFromWindowId(WindowId { Value: window_handle as u64 }).map_err(|error| format!("unable to create capture item: {error}"))?;
    let mut device: Option<ID3D11Device> = None;
    unsafe { D3D11CreateDevice(None, D3D_DRIVER_TYPE_HARDWARE, HMODULE::default(), D3D11_CREATE_DEVICE_FLAG(0), Some(&[D3D_FEATURE_LEVEL_11_0]), 7, Some(&mut device), None, None).map_err(|error| format!("unable to create D3D11 device: {error}"))?; }
    let dxgi_device: IDXGIDevice = device.ok_or("D3D11 returned no device")?.cast().map_err(|error| format!("unable to cast DXGI device: {error}"))?;
    let inspectable = unsafe { CreateDirect3D11DeviceFromDXGIDevice(&dxgi_device).map_err(|error| format!("unable to create WinRT Direct3D device: {error}"))? };
    let direct3d_device: IDirect3DDevice = inspectable.cast().map_err(|error| format!("unable to cast WinRT Direct3D device: {error}"))?;
    let frame_pool = Direct3D11CaptureFramePool::CreateFreeThreaded(&direct3d_device, DirectXPixelFormat::B8G8R8A8UIntNormalized, 2, item.Size().map_err(|error| format!("unable to read capture size: {error}"))?).map_err(|error| format!("unable to create capture frame pool: {error}"))?;
    let session = frame_pool.CreateCaptureSession(&item).map_err(|error| format!("unable to create capture session: {error}"))?;
    session.StartCapture().map_err(|error| format!("unable to start capture session: {error}"))?;
    Ok(D3d11CaptureSession { frame_pool, session })
}

#[cfg(not(windows))]
pub fn initialize(_window_handle: isize) -> Result<(), String> { Err("D3D11 capture is only available on Windows".into()) }
