//! Frames to the window from a thread of their own (native builds). The game draws each frame into an image of its
//! own and hands it over; this thread copies the newest image to the window, so whatever showing it waits for never
//! holds up the game's loop, whose steps, input and sound go on at their own pace.
//!
//! The window is a wgpu surface (its next buffer waited for, as long as the system makes it), or on macOS a layer
//! shown IOSurfaces of the game's own (`mac_layer`: a screen recording there holds a surface's buffers up to a second
//! each, never these), paced to the display's refresh rate with vsync on.
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// The images frames are drawn into: the one being copied to the window, the newest finished, the one being drawn.
const SLOTS: usize = 3;

/// Where frames go.
pub(crate) enum Output {
    Surface(wgpu::Surface<'static>),
    /// The layer, and the wgpu surface the adapter was chosen for (kept alive under it, never configured).
    #[cfg(target_os = "macos")]
    Layer(crate::mac_layer::Layer, #[allow(dead_code)] wgpu::Surface<'static>),
}

#[derive(Default)]
struct State {
    slots: [Option<wgpu::Texture>; SLOTS],
    /// The newest finished frame, not yet taken.
    latest: Option<usize>,
    /// The frame being copied to the window.
    copying: Option<usize>,
    /// The window's configuration, to apply before the next frame (the size, vsync).
    config: Option<wgpu::SurfaceConfiguration>,
    quit: bool,
}

type Taken = Box<dyn Fn() + Send + Sync>;

struct Shared {
    state: Mutex<State>,
    wake: Condvar,
    /// Called on the thread when it takes a frame: the game may draw the next one.
    taken: Mutex<Option<Taken>>,
}

pub(crate) struct Presenter {
    shared: Arc<Shared>,
    thread: Option<JoinHandle<()>>,
}

impl Presenter {
    /// The thread for `output`, configured as `config` (a surface's usage including COPY_DST) before the first frame;
    /// None if it cannot be started.
    pub fn new(output: Output, device: wgpu::Device, queue: wgpu::Queue, config: wgpu::SurfaceConfiguration) -> Option<Self> {
        let shared = Arc::new(Shared { state: Mutex::new(State { config: Some(config), ..Default::default() }), wake: Condvar::new(), taken: Mutex::new(None) });
        let s = shared.clone();
        let thread = std::thread::Builder::new().name("present".into()).spawn(move || run(&s, &output, &device, &queue)).ok()?;
        Some(Self { shared, thread: Some(thread) })
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.shared.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The window as `config` from the next frame on.
    pub fn configure(&self, config: &wgpu::SurfaceConfiguration) {
        self.lock().config = Some(config.clone());
    }

    /// Whether the last frame handed over has been taken: a frame drawn now is shown, not dropped for a newer one.
    pub fn ready(&self) -> bool {
        self.lock().latest.is_none()
    }

    pub fn on_taken(&self, f: Taken) {
        *self.shared.taken.lock().unwrap_or_else(|e| e.into_inner()) = Some(f);
    }

    /// An image to draw the next frame into, made (again) for `config`'s size and format: its slot and texture.
    pub fn slot(&self, device: &wgpu::Device, config: &wgpu::SurfaceConfiguration) -> (usize, wgpu::Texture) {
        let mut st = self.lock();
        // (three slots, two of them taken at most: one is free)
        let i = (0..SLOTS).find(|&i| Some(i) != st.latest && Some(i) != st.copying).unwrap_or(0);
        if let Some(t) = &st.slots[i] {
            if t.width() == config.width && t.height() == config.height && t.format() == config.format {
                return (i, t.clone());
            }
        }
        let t = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("frame"),
            size: wgpu::Extent3d { width: config.width, height: config.height, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: config.format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &config.view_formats,
        });
        st.slots[i] = Some(t.clone());
        (i, t)
    }

    /// Hand over the frame drawn into slot `i` (its commands submitted): the next one shown. One not yet taken is
    /// dropped for it.
    pub fn publish(&self, i: usize) {
        self.lock().latest = Some(i);
        self.shared.wake.notify_one();
    }
}

impl Drop for Presenter {
    fn drop(&mut self) {
        self.lock().quit = true;
        self.shared.wake.notify_one();
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

fn run(shared: &Shared, output: &Output, device: &wgpu::Device, queue: &wgpu::Queue) {
    let mut config = None;
    #[cfg(target_os = "macos")]
    let mut pool = crate::mac_layer::Pool::default();
    // the next refresh a layer's frame waits for (vsync on)
    #[cfg(target_os = "macos")]
    let mut next = Instant::now();
    loop {
        let (i, texture, new_config) = {
            let mut st = shared.state.lock().unwrap_or_else(|e| e.into_inner());
            let i = loop {
                if st.quit {
                    return;
                }
                if let Some(i) = st.latest.take() {
                    break i;
                }
                st = shared.wake.wait(st).unwrap_or_else(|e| e.into_inner());
            };
            st.copying = Some(i);
            (i, st.slots[i].clone(), st.config.take())
        };
        if let Some(c) = new_config {
            if let Output::Surface(s) = output {
                s.configure(device, &c);
            }
            config = Some(c);
        }
        if let Some(f) = shared.taken.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
            f();
        }
        // (drawn at the size before a resize: the next one fits)
        let fits = |t: &wgpu::Texture, c: &wgpu::SurfaceConfiguration| t.width() == c.width && t.height() == c.height;
        let shown = match (texture, &config, output) {
            (Some(t), Some(c), Output::Surface(s)) if fits(&t, c) => show(s, device, queue, c, &t),
            #[cfg(target_os = "macos")]
            (Some(t), Some(c), Output::Layer(layer, _)) if fits(&t, c) => {
                let vsync = matches!(c.present_mode, wgpu::PresentMode::AutoVsync | wgpu::PresentMode::Fifo | wgpu::PresentMode::FifoRelaxed);
                show_layer(layer, &mut pool, device, queue, &t, vsync.then_some(&mut next))
            }
            _ => true,
        };
        if !shown {
            // (the window hidden: frames a few times a second, not as fast as they can be drawn)
            std::thread::sleep(Duration::from_millis(100));
        }
        let mut st = shared.state.lock().unwrap_or_else(|e| e.into_inner());
        if st.copying == Some(i) {
            st.copying = None;
        }
    }
}

/// `texture` into the window's next buffer, presented; false when the window is hidden.
fn show(surface: &wgpu::Surface<'static>, device: &wgpu::Device, queue: &wgpu::Queue, config: &wgpu::SurfaceConfiguration, texture: &wgpu::Texture) -> bool {
    match surface.get_current_texture() {
        wgpu::CurrentSurfaceTexture::Success(frame) | wgpu::CurrentSurfaceTexture::Suboptimal(frame) => {
            let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("present") });
            enc.copy_texture_to_texture(texture.as_image_copy(), frame.texture.as_image_copy(), texture.size());
            queue.submit([enc.finish()]);
            queue.present(frame);
        }
        wgpu::CurrentSurfaceTexture::Occluded => return false,
        other => {
            log::warn!("surface: {other:?}; reconfiguring");
            surface.configure(device, config);
        }
    }
    true
}

/// `texture` copied into a free IOSurface, shown on the layer once drawn, at the next refresh with vsync (`next`);
/// false when the window is hidden.
#[cfg(target_os = "macos")]
fn show_layer(layer: &crate::mac_layer::Layer, pool: &mut crate::mac_layer::Pool, device: &wgpu::Device, queue: &wgpu::Queue, texture: &wgpu::Texture, next: Option<&mut Instant>) -> bool {
    if !layer.visible() {
        return false;
    }
    // (every surface still held by the window server or a recorder: this frame is skipped)
    let Some(b) = pool.free(device, [texture.width(), texture.height()]) else { return true };
    let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("present") });
    enc.copy_texture_to_texture(texture.as_image_copy(), pool.get(b).texture.as_image_copy(), texture.size());
    let done = queue.submit([enc.finish()]);
    // (shown once drawn: the window server reads the surface as it is)
    let _ = device.poll(wgpu::PollType::Wait { submission_index: Some(done), timeout: Some(Duration::from_secs(1)) });
    if let Some(next) = next {
        let now = Instant::now();
        if *next > now {
            std::thread::sleep(*next - now);
        }
        let interval = Duration::from_secs(1) / layer.hz().max(1);
        // (fallen behind: from now on)
        *next = (*next + interval).max(Instant::now());
    }
    layer.show(&pool.get(b).surface);
    pool.shown(b);
    true
}
