//! The Direct3D 11 side of capturing: the device the capture runs on, and turning a captured
//! texture into BGRA bytes of the size we want.
//!
//! Scaling happens on the GPU with the D3D11 video processor (copy the frame into a texture
//! we own, `VideoProcessorBlt` it into one of the output size, copy that to a staging texture).
//! When nothing needs scaling the frame goes straight to staging. Only if the driver has no
//! usable video processor is the full frame read back and shrunk on the CPU.

use std::mem::ManuallyDrop;
use windows::core::{Interface, Result};
use windows::Graphics::DirectX::Direct3D11::IDirect3DDevice;
use windows::Win32::Foundation::{HMODULE, RECT};
use windows::Win32::Graphics::Direct3D::{
    D3D_DRIVER_TYPE, D3D_DRIVER_TYPE_HARDWARE, D3D_DRIVER_TYPE_WARP,
};
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_RATIONAL, DXGI_SAMPLE_DESC,
};
use windows::Win32::Graphics::Dxgi::IDXGIDevice;
use windows::Win32::System::WinRT::Direct3D11::CreateDirect3D11DeviceFromDXGIDevice;

/// A D3D11 device plus its WinRT wrapper (what the capture frame pool wants).
pub struct Gpu {
    pub device: ID3D11Device,
    pub context: ID3D11DeviceContext,
    pub winrt: IDirect3DDevice,
}

impl Gpu {
    /// A hardware device with video support if possible, else plain hardware, else WARP.
    pub fn create() -> std::result::Result<Gpu, String> {
        let attempts: [(D3D_DRIVER_TYPE, D3D11_CREATE_DEVICE_FLAG); 3] = [
            (
                D3D_DRIVER_TYPE_HARDWARE,
                D3D11_CREATE_DEVICE_BGRA_SUPPORT | D3D11_CREATE_DEVICE_VIDEO_SUPPORT,
            ),
            (D3D_DRIVER_TYPE_HARDWARE, D3D11_CREATE_DEVICE_BGRA_SUPPORT),
            (D3D_DRIVER_TYPE_WARP, D3D11_CREATE_DEVICE_BGRA_SUPPORT),
        ];
        let mut last = String::from("no Direct3D device");
        for (driver, flags) in attempts {
            match unsafe { Self::create_with(driver, flags) } {
                Ok(gpu) => return Ok(gpu),
                Err(e) => last = format!("could not create a Direct3D 11 device: {e}"),
            }
        }
        Err(last)
    }

    unsafe fn create_with(driver: D3D_DRIVER_TYPE, flags: D3D11_CREATE_DEVICE_FLAG) -> Result<Gpu> {
        let mut device = None;
        let mut context = None;
        D3D11CreateDevice(
            None,
            driver,
            HMODULE::default(),
            flags,
            None,
            D3D11_SDK_VERSION,
            Some(&mut device),
            None,
            Some(&mut context),
        )?;
        let device: ID3D11Device = device.ok_or_else(windows::core::Error::empty)?;
        let context: ID3D11DeviceContext = context.ok_or_else(windows::core::Error::empty)?;
        // The capture frame pool uses the device from its own threads.
        if let Ok(mt) = context.cast::<ID3D11Multithread>() {
            let _ = mt.SetMultithreadProtected(true);
        }
        let dxgi: IDXGIDevice = device.cast()?;
        let winrt: IDirect3DDevice = CreateDirect3D11DeviceFromDXGIDevice(&dxgi)?.cast()?;
        Ok(Gpu {
            device,
            context,
            winrt,
        })
    }

    pub fn texture(
        &self,
        width: u32,
        height: u32,
        usage: D3D11_USAGE,
        bind: D3D11_BIND_FLAG,
        cpu: D3D11_CPU_ACCESS_FLAG,
    ) -> Result<ID3D11Texture2D> {
        let desc = D3D11_TEXTURE2D_DESC {
            Width: width,
            Height: height,
            MipLevels: 1,
            ArraySize: 1,
            Format: DXGI_FORMAT_B8G8R8A8_UNORM,
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            Usage: usage,
            BindFlags: bind.0 as u32,
            CPUAccessFlags: cpu.0 as u32,
            MiscFlags: 0,
        };
        let mut texture = None;
        unsafe {
            self.device
                .CreateTexture2D(&desc, None, Some(&mut texture))?
        };
        texture.ok_or_else(windows::core::Error::empty)
    }

    pub fn staging(&self, width: u32, height: u32) -> Result<ID3D11Texture2D> {
        self.texture(
            width,
            height,
            D3D11_USAGE_STAGING,
            D3D11_BIND_FLAG(0),
            D3D11_CPU_ACCESS_READ,
        )
    }

    /// Copy the `width`×`height` top-left part of `src` to the top-left of `dst`.
    pub fn copy_region(
        &self,
        dst: &ID3D11Texture2D,
        src: &ID3D11Texture2D,
        width: u32,
        height: u32,
    ) {
        let region = D3D11_BOX {
            left: 0,
            top: 0,
            front: 0,
            right: width,
            bottom: height,
            back: 1,
        };
        unsafe {
            self.context
                .CopySubresourceRegion(dst, 0, 0, 0, 0, src, 0, Some(&region));
        }
    }

    /// Read a staging texture's top-left `width`×`height` into `dst` (rows `dst_stride` apart).
    pub fn read_staging(
        &self,
        staging: &ID3D11Texture2D,
        width: u32,
        height: u32,
        dst: &mut [u8],
        dst_stride: usize,
    ) -> Result<()> {
        self.with_mapped(staging, |src, pitch| {
            let row = width as usize * 4;
            for y in 0..height as usize {
                let from = &src[y * pitch..y * pitch + row];
                dst[y * dst_stride..y * dst_stride + row].copy_from_slice(from);
            }
        })
    }

    /// Map a staging texture for reading and hand its bytes and row pitch to `f`.
    pub fn with_mapped<R>(
        &self,
        staging: &ID3D11Texture2D,
        f: impl FnOnce(&[u8], usize) -> R,
    ) -> Result<R> {
        let mut desc = D3D11_TEXTURE2D_DESC::default();
        unsafe { staging.GetDesc(&mut desc) };
        let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
        unsafe {
            self.context
                .Map(staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped))?
        };
        let pitch = mapped.RowPitch as usize;
        let len = pitch * desc.Height as usize;
        let bytes = unsafe { std::slice::from_raw_parts(mapped.pData as *const u8, len) };
        let result = f(bytes, pitch);
        unsafe { self.context.Unmap(staging, 0) };
        Ok(result)
    }
}

/// Turns captured frames into BGRA bytes of a given size, keeping its textures between frames.
pub struct Pipeline {
    key: (u32, u32, u32, u32),
    mode: Mode,
    /// The last result (output size, or input size in CPU mode); read again for repeats.
    staging: Option<ID3D11Texture2D>,
    /// The video processor failed once; do not try again for this capture.
    no_video_processor: bool,
}

enum Mode {
    None,
    /// Output size equals the (cropped) input: one copy to staging.
    Copy,
    /// GPU scaling.
    Scale(Box<Scaler>),
    /// Read the full frame back, shrink it on the CPU.
    Cpu,
}

struct Scaler {
    video: ID3D11VideoContext,
    processor: ID3D11VideoProcessor,
    input: ID3D11Texture2D,
    input_view: ID3D11VideoProcessorInputView,
    output: ID3D11Texture2D,
    output_view: ID3D11VideoProcessorOutputView,
}

impl Default for Pipeline {
    fn default() -> Self {
        Pipeline {
            key: (0, 0, 0, 0),
            mode: Mode::None,
            staging: None,
            no_video_processor: false,
        }
    }
}

impl Pipeline {
    /// Put the top-left `in_w`×`in_h` of `frame`, scaled to `out_w`×`out_h`, into staging.
    pub fn render(
        &mut self,
        gpu: &Gpu,
        frame: &ID3D11Texture2D,
        in_w: u32,
        in_h: u32,
        out_w: u32,
        out_h: u32,
    ) -> std::result::Result<(), String> {
        let key = (in_w, in_h, out_w, out_h);
        if key != self.key || matches!(self.mode, Mode::None) {
            self.rebuild(gpu, key)?;
        }
        let staging = self.staging.as_ref().ok_or("no staging texture")?;
        match &self.mode {
            Mode::Copy | Mode::Cpu => gpu.copy_region(staging, frame, in_w, in_h),
            Mode::Scale(s) => {
                gpu.copy_region(&s.input, frame, in_w, in_h);
                let stream = D3D11_VIDEO_PROCESSOR_STREAM {
                    Enable: true.into(),
                    pInputSurface: ManuallyDrop::new(Some(s.input_view.clone())),
                    ..Default::default()
                };
                let result = unsafe {
                    s.video.VideoProcessorBlt(
                        &s.processor,
                        &s.output_view,
                        0,
                        std::slice::from_ref(&stream),
                    )
                };
                drop(ManuallyDrop::into_inner(stream.pInputSurface));
                result.map_err(|e| format!("video processor blit failed: {e}"))?;
                unsafe { gpu.context.CopyResource(staging, &s.output) };
            }
            Mode::None => return Err("no capture pipeline".into()),
        }
        Ok(())
    }

    /// Whether there is a rendered frame to read (again).
    pub fn has_frame(&self) -> bool {
        self.staging.is_some() && !matches!(self.mode, Mode::None)
    }

    /// The output size of the last render.
    pub fn output_size(&self) -> (u32, u32) {
        (self.key.2, self.key.3)
    }

    /// Copy the last rendered frame into `dst`, rows `dst_stride` apart.
    pub fn read(
        &self,
        gpu: &Gpu,
        dst: &mut [u8],
        dst_stride: usize,
    ) -> std::result::Result<(), String> {
        let staging = self.staging.as_ref().ok_or("no staging texture")?;
        let (in_w, in_h, out_w, out_h) = self.key;
        let result = match self.mode {
            Mode::Cpu => gpu.with_mapped(staging, |src, pitch| {
                downscale_bgra(src, in_w, in_h, pitch, dst, out_w, out_h, dst_stride)
            }),
            _ => gpu.read_staging(staging, out_w, out_h, dst, dst_stride),
        };
        result.map_err(|e| format!("could not read the frame back: {e}"))
    }

    fn rebuild(&mut self, gpu: &Gpu, key: (u32, u32, u32, u32)) -> std::result::Result<(), String> {
        let (in_w, in_h, out_w, out_h) = key;
        self.mode = Mode::None;
        self.staging = None;
        self.key = key;
        if (in_w, in_h) == (out_w, out_h) {
            self.staging = Some(gpu.staging(out_w, out_h).map_err(|e| e.to_string())?);
            self.mode = Mode::Copy;
            return Ok(());
        }
        if !self.no_video_processor {
            match unsafe { Scaler::create(gpu, in_w, in_h, out_w, out_h) } {
                Ok(scaler) => {
                    self.staging = Some(gpu.staging(out_w, out_h).map_err(|e| e.to_string())?);
                    self.mode = Mode::Scale(Box::new(scaler));
                    return Ok(());
                }
                Err(e) => {
                    eprintln!("[capture] no GPU video processor ({e}); scaling on the CPU");
                    self.no_video_processor = true;
                }
            }
        }
        self.staging = Some(gpu.staging(in_w, in_h).map_err(|e| e.to_string())?);
        self.mode = Mode::Cpu;
        Ok(())
    }
}

impl Scaler {
    unsafe fn create(gpu: &Gpu, in_w: u32, in_h: u32, out_w: u32, out_h: u32) -> Result<Scaler> {
        let device: ID3D11VideoDevice = gpu.device.cast()?;
        let video: ID3D11VideoContext = gpu.context.cast()?;
        let rate = DXGI_RATIONAL {
            Numerator: 60,
            Denominator: 1,
        };
        let desc = D3D11_VIDEO_PROCESSOR_CONTENT_DESC {
            InputFrameFormat: D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE,
            InputFrameRate: rate,
            InputWidth: in_w,
            InputHeight: in_h,
            OutputFrameRate: rate,
            OutputWidth: out_w,
            OutputHeight: out_h,
            Usage: D3D11_VIDEO_USAGE_OPTIMAL_QUALITY,
        };
        let enumerator = device.CreateVideoProcessorEnumerator(&desc)?;
        let support = enumerator.CheckVideoProcessorFormat(DXGI_FORMAT_B8G8R8A8_UNORM)?;
        let needed = (D3D11_VIDEO_PROCESSOR_FORMAT_SUPPORT_INPUT.0
            | D3D11_VIDEO_PROCESSOR_FORMAT_SUPPORT_OUTPUT.0) as u32;
        if support & needed != needed {
            return Err(windows::core::Error::new(
                windows::Win32::Foundation::E_NOTIMPL,
                "BGRA is not supported by the video processor",
            ));
        }
        let processor = device.CreateVideoProcessor(&enumerator, 0)?;

        let render = D3D11_BIND_FLAG(D3D11_BIND_RENDER_TARGET.0 | D3D11_BIND_SHADER_RESOURCE.0);
        let input = gpu.texture(
            in_w,
            in_h,
            D3D11_USAGE_DEFAULT,
            render,
            D3D11_CPU_ACCESS_FLAG(0),
        )?;
        let output = gpu.texture(
            out_w,
            out_h,
            D3D11_USAGE_DEFAULT,
            D3D11_BIND_RENDER_TARGET,
            D3D11_CPU_ACCESS_FLAG(0),
        )?;

        let input_desc = D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC {
            FourCC: 0,
            ViewDimension: D3D11_VPIV_DIMENSION_TEXTURE2D,
            Anonymous: D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC_0 {
                Texture2D: D3D11_TEX2D_VPIV {
                    MipSlice: 0,
                    ArraySlice: 0,
                },
            },
        };
        let mut input_view = None;
        device.CreateVideoProcessorInputView(
            &input,
            &enumerator,
            &input_desc,
            Some(&mut input_view),
        )?;
        let input_view = input_view.ok_or_else(windows::core::Error::empty)?;

        let output_desc = D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC {
            ViewDimension: D3D11_VPOV_DIMENSION_TEXTURE2D,
            Anonymous: D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC_0 {
                Texture2D: D3D11_TEX2D_VPOV { MipSlice: 0 },
            },
        };
        let mut output_view = None;
        device.CreateVideoProcessorOutputView(
            &output,
            &enumerator,
            &output_desc,
            Some(&mut output_view),
        )?;
        let output_view = output_view.ok_or_else(windows::core::Error::empty)?;

        let src = RECT {
            left: 0,
            top: 0,
            right: in_w as i32,
            bottom: in_h as i32,
        };
        let dst = RECT {
            left: 0,
            top: 0,
            right: out_w as i32,
            bottom: out_h as i32,
        };
        // Full-range RGB in and out (the zero colour space), progressive, no "enhancements".
        let rgb_full = D3D11_VIDEO_PROCESSOR_COLOR_SPACE::default();
        video.VideoProcessorSetStreamFrameFormat(
            &processor,
            0,
            D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE,
        );
        video.VideoProcessorSetStreamColorSpace(&processor, 0, &rgb_full);
        video.VideoProcessorSetOutputColorSpace(&processor, &rgb_full);
        video.VideoProcessorSetStreamAutoProcessingMode(&processor, 0, false);
        video.VideoProcessorSetStreamSourceRect(&processor, 0, true, Some(&src));
        video.VideoProcessorSetStreamDestRect(&processor, 0, true, Some(&dst));
        video.VideoProcessorSetOutputTargetRect(&processor, true, Some(&dst));

        Ok(Scaler {
            video,
            processor,
            input,
            input_view,
            output,
            output_view,
        })
    }
}

/// Shrink a BGRA image by averaging the source pixels under each destination pixel.
#[allow(clippy::too_many_arguments)]
pub fn downscale_bgra(
    src: &[u8],
    src_w: u32,
    src_h: u32,
    src_stride: usize,
    dst: &mut [u8],
    dst_w: u32,
    dst_h: u32,
    dst_stride: usize,
) {
    let (sw, sh, dw, dh) = (
        src_w as usize,
        src_h as usize,
        dst_w as usize,
        dst_h as usize,
    );
    if sw == 0 || sh == 0 || dw == 0 || dh == 0 {
        return;
    }
    // Column spans are the same for every row.
    let spans: Vec<(usize, usize)> = (0..dw)
        .map(|x| {
            let x0 = x * sw / dw;
            let x1 = ((x + 1) * sw / dw).max(x0 + 1).min(sw);
            (x0, x1)
        })
        .collect();
    for y in 0..dh {
        let y0 = y * sh / dh;
        let y1 = ((y + 1) * sh / dh).max(y0 + 1).min(sh);
        let out = &mut dst[y * dst_stride..y * dst_stride + dw * 4];
        for (x, &(x0, x1)) in spans.iter().enumerate() {
            let mut sum = [0u32; 4];
            for row in y0..y1 {
                let line = &src[row * src_stride + x0 * 4..row * src_stride + x1 * 4];
                for px in line.chunks_exact(4) {
                    sum[0] += px[0] as u32;
                    sum[1] += px[1] as u32;
                    sum[2] += px[2] as u32;
                    sum[3] += px[3] as u32;
                }
            }
            let n = ((x1 - x0) * (y1 - y0)) as u32;
            let o = &mut out[x * 4..x * 4 + 4];
            for c in 0..4 {
                o[c] = (sum[c] / n) as u8;
            }
        }
    }
}
