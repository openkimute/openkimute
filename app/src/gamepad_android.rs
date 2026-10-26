//! Gamepads on Android, as the native ones: their buttons and d-pad come as the window's key events (a stick or a
//! hat as d-pad keys: vendor/winit), not through gilrs. A pad joins with its first gamepad button (the first plays
//! P1, the second P2, the tag brawl: up to P4); its d-pad is then its own, not the keyboard's arrows. The buttons do
//! what the player's KEY CONFIGURATION says. The on-screen pad (touchpad.rs) is one more pad, joining the same way.

use crate::controls::{merge, raw_menu_key, Controls, PadButton, Step};
use crate::menu::Key;
use crate::touchpad::TouchPad;
use kumite_sim::input::Pad;
use winit::event::DeviceId;
use winit::keyboard::{KeyCode, NativeKeyCode, PhysicalKey};

/// A key's pad button: Android's KEYCODE_BUTTON_A.. (96..110), and the d-pad (winit's arrows).
fn pad_button(key: PhysicalKey) -> Option<PadButton> {
    Some(match key {
        PhysicalKey::Code(KeyCode::ArrowUp) => PadButton::Up,
        PhysicalKey::Code(KeyCode::ArrowDown) => PadButton::Down,
        PhysicalKey::Code(KeyCode::ArrowLeft) => PadButton::Left,
        PhysicalKey::Code(KeyCode::ArrowRight) => PadButton::Right,
        PhysicalKey::Unidentified(NativeKeyCode::Android(code)) => match code {
            96 => PadButton::South,
            97 => PadButton::East,
            99 => PadButton::West,
            100 => PadButton::North,
            102 => PadButton::L1,
            103 => PadButton::R1,
            104 => PadButton::L2,
            105 => PadButton::R2,
            106 => PadButton::L3,
            107 => PadButton::R3,
            108 => PadButton::Start,
            109 => PadButton::Select,
            110 => PadButton::Home,
            _ => return None,
        },
        _ => return None,
    })
}

pub struct Gamepads {
    /// The pads in the order they joined (index = player), each with the buttons it holds; None: the on-screen pad.
    // shortcut: a pad that reconnects (a new device id) joins as the next player; upgrade with Android's
    // InputManager device listener (JNI) to give it the gone pad's seat
    pads: Vec<(Option<DeviceId>, Vec<PadButton>)>,
    /// The on-screen pad: shown until a gamepad plays, back with the next touch.
    pub touch: TouchPad,
    /// Buttons pressed since the last poll (player, button).
    pressed: Vec<(usize, PadButton)>,
    /// Buttons/directions pressed since the last step (a tap shorter than a frame is not lost).
    tapped: [Pad; 4],
    /// The sidestep buttons, per player.
    steps: [Step; 4],
}

impl Gamepads {
    pub fn new() -> Self {
        Self { pads: Vec::new(), touch: TouchPad::default(), pressed: Vec::new(), tapped: [Pad::default(); 4], steps: [Step::default(); 4] }
    }

    /// The seat of pad `device`; a new one for a press (a pad joins with its first press).
    fn seat(&mut self, device: Option<DeviceId>, press: bool) -> Option<usize> {
        if let Some(p) = self.pads.iter().position(|(d, _)| *d == device) {
            return Some(p);
        }
        if !press {
            return None;
        }
        self.pads.push((device, Vec::new()));
        log::info!("gamepad {} plays P{}", device.map_or("on the screen".to_string(), |d| format!("{d:?}")), self.pads.len());
        Some(self.pads.len() - 1)
    }

    /// A key from `device`: taken (true) when it is a gamepad's (which hides the on-screen pad).
    pub fn key(&mut self, device: DeviceId, key: PhysicalKey, pressed: bool, repeat: bool) -> bool {
        let Some(b) = pad_button(key) else { return false };
        // (arrows from a device that is not a pad yet: a keyboard's)
        let Some(p) = self.seat(Some(device), pressed && !matches!(key, PhysicalKey::Code(_))) else { return false };
        self.touch.shown = false;
        let held = &mut self.pads[p].1;
        if !pressed {
            held.retain(|&h| h != b);
        } else if !repeat && !held.contains(&b) {
            held.push(b);
            self.pressed.push((p, b));
        }
        true
    }

    /// A finger on the screen (`w` x `h` physical pixels, `dp` to a dp): taken (true) when it is the on-screen pad's.
    #[allow(clippy::too_many_arguments)] // one touch, and the screen it is on
    pub fn finger(&mut self, id: u64, phase: winit::event::TouchPhase, x: f32, y: f32, w: f32, h: f32, dp: f32) -> bool {
        if !self.touch.touch(id, phase, x, y, w, h, dp) {
            return false;
        }
        let now = self.touch.held(dp);
        let Some(p) = self.seat(None, !now.is_empty()) else { return true };
        let held = &mut self.pads[p].1;
        self.pressed.extend(now.iter().filter(|b| !held.contains(b)).map(|&b| (p, b)));
        *held = now;
        true
    }

    /// Every finger off the on-screen pad (the app in the background: no lift will come for those down).
    pub fn lift_fingers(&mut self) {
        self.touch.lift();
        if let Some((_, held)) = self.pads.iter_mut().find(|(d, _)| d.is_none()) {
            held.clear();
        }
    }

    /// The buttons pressed since the last poll (player, button, its menu key); taps latched for the next step.
    pub fn poll(&mut self, controls: &Controls) -> Vec<(usize, PadButton, Option<Key>)> {
        let pressed = std::mem::take(&mut self.pressed);
        pressed
            .into_iter()
            .filter(|&(p, _)| p < 4)
            .map(|(p, b)| {
                self.tapped[p] = merge(self.tapped[p], controls.pad(p).pad(std::iter::once(b), (0.0, 0.0)).0);
                (p, b, raw_menu_key(b))
            })
            .collect()
    }

    /// No rumble on Android yet.
    // shortcut: the pad's vibrator is Java's (InputDevice.getVibratorManager), upgrade through JNI
    pub fn rumble(&mut self, _p: usize, _large: u8, _small: f32) {}

    /// Each player's gamepad: the buttons held now (None: no pad).
    pub fn held(&self) -> [Option<Vec<PadButton>>; 4] {
        std::array::from_fn(|p| self.pads.get(p).map(|(_, h)| h.clone()))
    }

    /// Current pad of each player who has a gamepad, up to four (taps since the last call included).
    pub fn pads4(&mut self, controls: &Controls) -> [Option<Pad>; 4] {
        let mut out = [None; 4];
        for (player, (_, held)) in self.pads.iter().enumerate().take(4) {
            let (state, steps) = controls.pad(player).pad(held.iter().copied(), (0.0, 0.0));
            out[player] = Some(self.steps[player].apply(merge(state, self.tapped[player]), steps));
        }
        self.tapped = [Pad::default(); 4];
        out
    }
}
