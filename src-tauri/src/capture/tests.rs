//! Smoke tests that need a Windows desktop session; run with
//! `cargo test capture -- --ignored --nocapture --test-threads=1`.

use super::gpu::{Gpu, Pipeline};
use super::session::{create_item, fit, grab_still, ComGuard};
use super::sources::{self, Source};
use std::time::{Duration, Instant};
use windows::Graphics::Capture::Direct3D11CaptureFramePool;
use windows::Graphics::DirectX::DirectXPixelFormat;
use windows::Win32::Graphics::Direct3D11::{ID3D11Texture2D, D3D11_TEXTURE2D_DESC};
use windows::Win32::System::WinRT::Direct3D11::IDirect3DDxgiInterfaceAccess;
use windows_core::Interface;

#[test]
fn fit_keeps_aspect_and_even_sides() {
    assert_eq!(fit(1920, 1080, 1280, 720, 720), (1280, 720));
    assert_eq!(fit(1921, 1081, 1922, 1082, 0), (1920, 1080));
    assert_eq!(fit(1000, 2000, 1280, 720, 720), (360, 720));
    assert_eq!(fit(4000, 1000, 1280, 720, 720), (1280, 320));
    assert_eq!(fit(640, 480, 1280, 720, 720), (640, 480));
}

#[test]
fn source_ids_round_trip() {
    for source in [
        Source::Screen(65537),
        Source::Window(-12),
        Source::Window(1312),
    ] {
        assert_eq!(Source::parse(&source.id()), Some(source));
    }
    assert_eq!(Source::parse("tab:1"), None);
    assert_eq!(Source::parse("window:x"), None);
}

#[test]
#[ignore]
fn lists_sources_quickly() {
    for round in 0..3 {
        let started = Instant::now();
        let list = sources::list(320).expect("list failed");
        let took = started.elapsed();
        println!(
            "round {round}: {} sources in {} ms",
            list.len(),
            took.as_millis()
        );
        if round == 0 {
            for s in &list {
                println!(
                    "  {:<6} {:<28} {:>5}x{:<5} app={:?} icon={} thumb={} primary={}",
                    s.kind,
                    s.id,
                    s.width,
                    s.height,
                    s.app_name,
                    s.icon.as_ref().map_or(0, |i| i.len()),
                    s.thumb.as_ref().map_or(0, |t| t.len()),
                    s.primary,
                );
                println!("         {}", s.name);
            }
        }
        assert!(list.iter().any(|s| s.kind == "screen"));
    }
}

#[test]
#[ignore]
fn captures_and_scales_the_primary_monitor() {
    let _com = ComGuard::mta();
    let gpu = Gpu::create().expect("no gpu");
    let monitor = sources::monitors()
        .into_iter()
        .find(|m| m.primary)
        .expect("no primary monitor");
    let source = Source::Screen(monitor.handle);

    let started = Instant::now();
    let still = grab_still(&gpu, source, Duration::from_millis(500));
    println!(
        "still: {:?} in {} ms",
        still.as_ref().map(|(_, w, h)| (w, h)),
        started.elapsed().as_millis()
    );

    let item = create_item(source).expect("no item");
    let size = item.Size().expect("no size");
    let pool = Direct3D11CaptureFramePool::CreateFreeThreaded(
        &gpu.winrt,
        DirectXPixelFormat::B8G8R8A8UIntNormalized,
        2,
        size,
    )
    .expect("no pool");
    let session = pool.CreateCaptureSession(&item).expect("no session");
    let _ = session.SetIsBorderRequired(false);
    session.StartCapture().expect("no start");

    for max_height in [720u32, 0] {
        let mut pipeline = Pipeline::default();
        let (w, h) = (size.Width as u32, size.Height as u32);
        let (box_w, box_h) = if max_height > 0 && h > max_height {
            (((w * max_height).div_ceil(h) + 1) & !1, max_height)
        } else {
            (w, h)
        };
        let mut slot = vec![0u8; box_w as usize * 4 * box_h as usize];
        let mut frames = 0;
        let mut render = Duration::ZERO;
        let mut read = Duration::ZERO;
        let deadline = Instant::now() + Duration::from_millis(1500);
        while Instant::now() < deadline {
            let Ok(frame) = pool.TryGetNextFrame() else {
                std::thread::sleep(Duration::from_millis(2));
                continue;
            };
            let access: IDirect3DDxgiInterfaceAccess = frame.Surface().unwrap().cast().unwrap();
            let texture: ID3D11Texture2D = unsafe { access.GetInterface() }.unwrap();
            let mut desc = D3D11_TEXTURE2D_DESC::default();
            unsafe { texture.GetDesc(&mut desc) };
            let (cw, ch) = (desc.Width.min(w), desc.Height.min(h));
            let (ow, oh) = fit(cw, ch, box_w, box_h, max_height);
            let (iw, ih) = if (ow, oh) == (cw & !1, ch & !1) {
                (ow, oh)
            } else {
                (cw, ch)
            };
            let t = Instant::now();
            pipeline
                .render(&gpu, &texture, iw, ih, ow, oh)
                .expect("render failed");
            render += t.elapsed();
            let t = Instant::now();
            pipeline
                .read(&gpu, &mut slot, ow as usize * 4)
                .expect("read failed");
            read += t.elapsed();
            let _ = frame.Close();
            frames += 1;
        }
        let (ow, oh) = pipeline.output_size();
        println!(
            "max_height {max_height}: {frames} frames of {ow}x{oh}; per frame render {:.2} ms, read back {:.2} ms",
            render.as_secs_f64() * 1000.0 / f64::from(frames.max(1)),
            read.as_secs_f64() * 1000.0 / f64::from(frames.max(1)),
        );
        let lit = slot[..ow as usize * 4 * oh as usize]
            .chunks_exact(4)
            .filter(|p| p[0] | p[1] | p[2] != 0)
            .count();
        println!("  non-black pixels: {lit}");
    }
    let _ = session.Close();
    let _ = pool.Close();
}
