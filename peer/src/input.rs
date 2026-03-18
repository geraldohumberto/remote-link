use anyhow::Result;
use enigo::{
    Enigo, Settings, Keyboard, Mouse,
    Key, Button,
    Direction::{Press, Release, Click},
    Coordinate::Abs,
    Axis::Vertical,
};
use crate::protocol::{InputEvent, MouseBtn};

pub struct Injector { enigo: Enigo }

impl Injector {
    pub fn new() -> Result<Self> {
        Ok(Self {
            enigo: Enigo::new(&Settings::default())
                .map_err(|e| anyhow::anyhow!("{:?}", e))?,
        })
    }

    pub fn inject(&mut self, ev: &InputEvent) -> Result<()> {
        match ev {
            InputEvent::MouseMove { x, y } => { self.enigo.move_mouse(*x, *y, Abs)?; }
            InputEvent::MouseDown { x, y, button } => {
                self.enigo.move_mouse(*x, *y, Abs)?;
                self.enigo.button(to_btn(button), Press)?;
            }
            InputEvent::MouseUp { x, y, button } => {
                self.enigo.move_mouse(*x, *y, Abs)?;
                self.enigo.button(to_btn(button), Release)?;
            }
            InputEvent::MouseDbl { x, y } => {
                self.enigo.move_mouse(*x, *y, Abs)?;
                self.enigo.button(Button::Left, Click)?;
                self.enigo.button(Button::Left, Click)?;
            }
            InputEvent::Scroll { x, y, dy } => {
                self.enigo.move_mouse(*x, *y, Abs)?;
                self.enigo.scroll(*dy, Vertical)?;
            }
            InputEvent::KeyDown { key } => { if let Some(k) = map_key(key) { self.enigo.key(k, Press)?; } }
            InputEvent::KeyUp   { key } => { if let Some(k) = map_key(key) { self.enigo.key(k, Release)?; } }
            InputEvent::TypeText { text } => { self.enigo.text(text)?; }
        }
        Ok(())
    }
}

fn to_btn(b: &MouseBtn) -> Button {
    match b { MouseBtn::Left => Button::Left, MouseBtn::Middle => Button::Middle, MouseBtn::Right => Button::Right }
}

fn map_key(k: &str) -> Option<Key> {
    Some(match k {
        "enter" => Key::Return, "backspace" => Key::Backspace, "tab" => Key::Tab,
        "escape" => Key::Escape, "delete" => Key::Delete, "home" => Key::Home,
        "end" => Key::End, "pageup" => Key::PageUp, "pagedown" => Key::PageDown,
        "up" => Key::UpArrow, "down" => Key::DownArrow, "left" => Key::LeftArrow, "right" => Key::RightArrow,
        "f1" => Key::F1, "f2" => Key::F2, "f3" => Key::F3, "f4" => Key::F4,
        "f5" => Key::F5, "f6" => Key::F6, "f7" => Key::F7, "f8" => Key::F8,
        "f9" => Key::F9, "f10" => Key::F10, "f11" => Key::F11, "f12" => Key::F12,
        "ctrl" => Key::Control, "alt" => Key::Alt, "shift" => Key::Shift,
        "win" | "super" => Key::Meta, "space" => Key::Space,
        "capslock" => Key::CapsLock, "insert" => Key::Insert,
        _ => return None,
    })
}
