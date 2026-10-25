//! macOS: the window's frames as IOSurfaces set straight on a layer of the window's view (as browsers show theirs),
//! not through a CAMetalLayer's drawables. While anything records the screen, macOS hands a CAMetalLayer's drawables
//! back slowly, up to a second each (measured on macOS 27 with `screencapture -v` and ScreenCaptureKit: 3 to 50
//! frames a second, in a plain Metal app too). The IOSurfaces here are the game's own: one the window server or a
//! recorder still uses is passed over and another made, so showing a frame never waits; measured, 120 a second
//! on a 120 Hz screen while recording.
// Objective-C calls: Core Animation, IOSurface and Metal, and wgpu's import of a Metal texture.
#![allow(unsafe_code)]

use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{msg_send, MainThreadMarker};
use objc2_core_foundation::{CFDictionary, CFNumber, CFRetained, CFString, CFType};
use objc2_io_surface::{kIOSurfaceBytesPerElement, kIOSurfaceColorSpace, kIOSurfaceHeight, kIOSurfacePixelFormat, kIOSurfaceWidth, IOSurfaceRef};
use objc2_metal::{MTLDevice, MTLPixelFormat, MTLStorageMode, MTLTextureDescriptor, MTLTextureType, MTLTextureUsage};
use objc2_quartz_core::{kCAGravityResize, CAAutoresizingMask, CALayer, CATransaction};
use std::sync::atomic::{AtomicU32, Ordering};
use wgpu::rwh::{HasWindowHandle, RawWindowHandle};

/// The frames' pixel format: IOSurface's 'BGRA', the frame's bytes as drawn (sRGB-encoded).
pub(crate) const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Bgra8Unorm;
/// Surfaces at most: more are made only while a recorder holds on to the others.
const MAX: usize = 8;

/// The layer over the window's view, the window, and the display's refresh rate (what vsync paces frames to).
pub(crate) struct Layer {
    layer: Retained<CALayer>,
    window: Retained<AnyObject>,
    hz: AtomicU32,
}

// SAFETY: the layer is made and added on the main thread ([`Layer::new`]); the present thread only sets its contents,
// inside an explicit transaction it commits and flushes, which Core Animation allows from any thread, and reads the
// window's occlusion state (as wgpu's own Metal surface does from any thread).
unsafe impl Send for Layer {}
unsafe impl Sync for Layer {}

/// The NSView of `target`, if it is an AppKit window.
pub(crate) fn view_of(target: &wgpu::SurfaceTarget<'_>) -> Option<std::ptr::NonNull<std::ffi::c_void>> {
    let handle = match target {
        wgpu::SurfaceTarget::DisplayAndWindow(w) => w.window_handle().ok()?.as_raw(),
        wgpu::SurfaceTarget::Window(w) => w.window_handle().ok()?.as_raw(),
        #[allow(unreachable_patterns)]
        _ => return None,
    };
    match handle {
        RawWindowHandle::AppKit(h) => Some(h.ns_view),
        _ => None,
    }
}

impl Layer {
    /// A layer filling `view` (from [`view_of`]), on top of what it shows (added after wgpu's surface); None off the
    /// main thread, or without a window.
    pub fn new(view: std::ptr::NonNull<std::ffi::c_void>) -> Option<Self> {
        MainThreadMarker::new()?;
        // SAFETY: the window's NSView, alive as long as the window (which the surface target keeps); main thread.
        unsafe {
            let view: &AnyObject = view.cast().as_ref();
            let window: Option<Retained<AnyObject>> = msg_send![view, window];
            let window = window?;
            let _: () = msg_send![view, setWantsLayer: true];
            let host: Option<Retained<CALayer>> = msg_send![view, layer];
            let host = host?;
            let layer = CALayer::new();
            layer.setFrame(host.bounds());
            layer.setAutoresizingMask(CAAutoresizingMask::LayerWidthSizable | CAAutoresizingMask::LayerHeightSizable);
            layer.setContentsGravity(kCAGravityResize);
            layer.setOpaque(true);
            host.addSublayer(&layer);
            let this = Self { layer, window, hz: AtomicU32::new(60) };
            this.refresh(view);
            Some(this)
        }
    }

    /// The view's screen's refresh rate, read again (main thread: the window may have moved to another screen).
    fn refresh(&self, view: &AnyObject) {
        // SAFETY: main thread only (callers); `window` and `screen` may be nil.
        let hz: isize = unsafe {
            let window: Option<Retained<AnyObject>> = msg_send![view, window];
            let screen: Option<Retained<AnyObject>> = window.and_then(|w| msg_send![&*w, screen]);
            screen.map_or(0, |s| msg_send![&*s, maximumFramesPerSecond])
        };
        if hz > 0 {
            self.hz.store(hz as u32, Ordering::Relaxed);
        }
    }

    /// Whether any of the window shows on screen.
    pub fn visible(&self) -> bool {
        const VISIBLE: usize = 1 << 1; // NSWindowOcclusionStateVisible
                                       // SAFETY: a property read (see the Send note).
        let state: usize = unsafe { msg_send![&*self.window, occlusionState] };
        state & VISIBLE != 0
    }

    /// Frames a second with vsync on.
    pub fn hz(&self) -> u32 {
        self.hz.load(Ordering::Relaxed)
    }

    /// Show `surface` (its drawing finished).
    pub fn show(&self, surface: &IOSurfaceRef) {
        CATransaction::begin();
        CATransaction::setDisableActions(true);
        // SAFETY: an IOSurface is a layer's valid contents; explicit transaction, committed and flushed.
        unsafe { self.layer.setContents(Some(&*(surface as *const IOSurfaceRef).cast::<AnyObject>())) };
        CATransaction::commit();
        CATransaction::flush();
    }
}

/// One frame's IOSurface and the wgpu texture drawing into it.
pub(crate) struct Buffer {
    pub surface: CFRetained<IOSurfaceRef>,
    pub texture: wgpu::Texture,
}

/// The surfaces, each shown in turn: a free one (not the one on screen, not in use by the window server or a
/// recorder) for each frame.
#[derive(Default)]
pub(crate) struct Pool {
    buffers: Vec<Buffer>,
    size: [u32; 2],
    shown: Option<usize>,
}

impl Pool {
    /// A free surface of `size` (made if none is), or None when all are held.
    pub fn free(&mut self, device: &wgpu::Device, size: [u32; 2]) -> Option<usize> {
        if size != self.size {
            (self.buffers, self.size, self.shown) = (Vec::new(), size, None);
        }
        let free = (0..self.buffers.len()).find(|&i| Some(i) != self.shown && !self.buffers[i].surface.is_in_use());
        if free.is_some() {
            return free;
        }
        if self.buffers.len() >= MAX {
            return None;
        }
        self.buffers.push(make(device, size)?);
        Some(self.buffers.len() - 1)
    }

    pub fn get(&self, i: usize) -> &Buffer {
        &self.buffers[i]
    }

    pub fn shown(&mut self, i: usize) {
        self.shown = Some(i);
    }
}

/// An IOSurface of `width`×`height` BGRA (sRGB), and a wgpu texture on it.
fn make(device: &wgpu::Device, [width, height]: [u32; 2]) -> Option<Buffer> {
    let n = |v: i32| CFNumber::new_i32(v);
    let (w, h, bpe, fmt) = (n(width as i32), n(height as i32), n(4), n(i32::from_be_bytes(*b"BGRA")));
    // SAFETY: the keys are IOSurface's own constants.
    let keys: [&CFString; 4] = unsafe { [kIOSurfaceWidth, kIOSurfaceHeight, kIOSurfaceBytesPerElement, kIOSurfacePixelFormat] };
    let props = CFDictionary::<CFString, CFNumber>::from_slices(&keys, &[&*w, &*h, &*bpe, &*fmt]);
    // SAFETY: a dictionary of the documented keys and number values.
    let surface = unsafe { IOSurfaceRef::new(props.as_opaque()) }?;
    // the bytes are sRGB (as a CAMetalLayer's): said so, or macOS takes them as the display's colours
    // SAFETY: the documented key, a colour space name.
    unsafe { surface.set_value(kIOSurfaceColorSpace, &CFString::from_static_str("kCGColorSpaceSRGB") as &CFType) };
    // SAFETY: wgpu's Metal device, used only to make a texture on the surface.
    let hal = unsafe { device.as_hal::<wgpu::hal::api::Metal>() }?;
    let desc = MTLTextureDescriptor::new();
    // SAFETY: plain setters on a fresh descriptor.
    unsafe {
        desc.setTextureType(MTLTextureType::Type2D);
        desc.setPixelFormat(MTLPixelFormat::BGRA8Unorm);
        desc.setWidth(width as usize);
        desc.setHeight(height as usize);
        desc.setUsage(MTLTextureUsage::RenderTarget | MTLTextureUsage::ShaderRead);
        desc.setStorageMode(MTLStorageMode::Shared);
    }
    let raw = hal.raw_device().newTextureWithDescriptor_iosurface_plane(&desc, &surface, 0)?;
    drop(hal);
    let extent = wgpu::hal::CopyExtent { width, height, depth: 1 };
    // SAFETY: a fresh 2D texture of this format and size, one layer, one level.
    let hal_texture = unsafe { wgpu::hal::metal::Device::texture_from_raw(raw, FORMAT, MTLTextureType::Type2D, 1, 1, extent, None) };
    let desc = wgpu::TextureDescriptor {
        label: Some("window frame"),
        size: wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: FORMAT,
        usage: wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    };
    // SAFETY: the texture just made, matching `desc`; its contents undefined until the first copy.
    let texture = unsafe { device.create_texture_from_hal::<wgpu::hal::api::Metal>(hal_texture, &desc, wgpu::wgt::TextureUses::UNINITIALIZED) };
    Some(Buffer { surface, texture })
}
