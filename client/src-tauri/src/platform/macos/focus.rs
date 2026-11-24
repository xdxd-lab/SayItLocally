// [自訂] L-80：保存鍵盤焦點的原生參照，只在記憶體內使用，IPC 只傳權杖。
use super::{appkit, ax::AxElement};
use std::sync::{Arc, Mutex};

const AX_FOCUS_TIMEOUT_SECS: f32 = 0.015;

pub struct Snapshot<T> {
    pub pid: Option<i32>,
    pub window: Option<T>,
    pub element: Option<T>,
}

impl<T> Default for Snapshot<T> {
    fn default() -> Self {
        Self { pid: None, window: None, element: None }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FocusChange {
    Application,
    Window,
    Element,
    Expired,
}

impl FocusChange {
    pub fn code(self) -> &'static str {
        match self {
            Self::Application => "application_changed",
            Self::Window => "window_changed",
            Self::Element => "element_changed",
            Self::Expired => "target_expired",
        }
    }
}

impl<T> Snapshot<T> {
    fn change(&self, current: &Self, equal: impl Fn(&T, &T) -> bool) -> Option<FocusChange> {
        if self.pid.zip(current.pid).is_some_and(|(first, last)| first != last) {
            return Some(FocusChange::Application);
        }
        if self.window.as_ref().zip(current.window.as_ref()).is_some_and(|(first, last)| !equal(first, last)) {
            return Some(FocusChange::Window);
        }
        if self.element.as_ref().zip(current.element.as_ref()).is_some_and(|(first, last)| !equal(first, last)) {
            return Some(FocusChange::Element);
        }
        None
    }
}

struct SavedTarget {
    token: String,
    snapshot: Option<Arc<Snapshot<AxElement>>>,
}

static TARGET: Mutex<Option<SavedTarget>> = Mutex::new(None);

pub fn begin() -> String {
    let token = uuid::Uuid::new_v4().to_string();
    *TARGET.lock().unwrap_or_else(|error| error.into_inner()) =
        Some(SavedTarget { token: token.clone(), snapshot: None });
    token
}

pub fn save(token: &str, snapshot: Snapshot<AxElement>) {
    let mut slot = TARGET.lock().unwrap_or_else(|error| error.into_inner());
    if let Some(entry) = slot.as_mut().filter(|entry| entry.token == token) {
        entry.snapshot = Some(Arc::new(snapshot));
    }
}

pub fn release(token: &str) {
    let mut slot = TARGET.lock().unwrap_or_else(|error| error.into_inner());
    if slot.as_ref().is_some_and(|entry| entry.token == token) {
        *slot = None;
    }
}

fn target(token: &str) -> Result<Arc<Snapshot<AxElement>>, FocusChange> {
    let slot = TARGET.lock().unwrap_or_else(|error| error.into_inner());
    slot.as_ref().filter(|entry| entry.token == token)
        .and_then(|entry| entry.snapshot.clone()).ok_or(FocusChange::Expired)
}

/// Query the keyboard application's AX objects, never the topmost visual window.
/// The SayItLocally overlay is shown without taking keyboard focus.
pub fn keyboard_target(pid: i32, application: &AxElement) -> Snapshot<AxElement> {
    application.set_timeout(AX_FOCUS_TIMEOUT_SECS);
    read_keyboard_target(pid, |name| application.attr_element(name))
}

fn read_keyboard_target<T>(pid: i32, read: impl Fn(&str) -> Option<T>) -> Snapshot<T> {
    Snapshot {
        pid: Some(pid),
        window: read("AXFocusedWindow"),
        element: read("AXFocusedUIElement"),
    }
}

pub fn verify(token: Option<&str>, legacy_pid: i32) -> Result<(), FocusChange> {
    let original = token.map(target).transpose()?;
    let expected_pid = original.as_ref().and_then(|snapshot| snapshot.pid)
        .or_else(|| (legacy_pid > 0).then_some(legacy_pid));
    let mut current = Snapshot { pid: appkit::frontmost_pid(), ..Snapshot::default() };
    if expected_pid.zip(current.pid).is_some_and(|(first, last)| first != last) {
        return Err(FocusChange::Application);
    }

    if let Some(original) = original {
        if original.window.is_some() || original.element.is_some() {
            if let Some((pid, application)) = current.pid.and_then(|pid| Some((pid, AxElement::application(pid)?))) {
                current = keyboard_target(pid, &application);
            }
        }
        if let Some(change) = original.change(&current, AxElement::equals) {
            return Err(change);
        }
        // Foreground can change while AX is answering; use the freshest application signal.
        if expected_pid.zip(appkit::frontmost_pid()).is_some_and(|(first, last)| first != last) {
            return Err(FocusChange::Application);
        }
        // An abandoned or superseded run cannot proceed, even if it already cloned the AX refs.
        if let Some(token) = token {
            target(token)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snapshot(pid: Option<i32>, window: Option<i32>, element: Option<i32>) -> Snapshot<i32> {
        Snapshot { pid, window, element }
    }

    #[test]
    fn detects_each_known_change_and_uses_semantic_equality() {
        let original = snapshot(Some(20), Some(11), Some(12));
        assert_eq!(original.change(&snapshot(Some(20), Some(11), Some(12)), i32::eq), None);
        assert_eq!(original.change(&snapshot(Some(21), Some(11), Some(12)), i32::eq), Some(FocusChange::Application));
        assert_eq!(original.change(&snapshot(Some(20), Some(13), Some(12)), i32::eq), Some(FocusChange::Window));
        assert_eq!(original.change(&snapshot(Some(20), Some(11), Some(14)), i32::eq), Some(FocusChange::Element));
    }

    #[test]
    fn unavailable_signals_do_not_hide_changes_in_other_fields() {
        let original = snapshot(Some(20), Some(11), Some(12));
        for current in [snapshot(None, Some(11), Some(12)), snapshot(Some(20), None, Some(12)),
            snapshot(Some(20), Some(11), None), Snapshot::default()] {
            assert_eq!(original.change(&current, i32::eq), None);
            assert_eq!(current.change(&original, i32::eq), None);
        }
        assert_eq!(original.change(&snapshot(None, None, Some(14)), i32::eq), Some(FocusChange::Element));
        assert_eq!(original.change(&snapshot(Some(21), None, None), i32::eq), Some(FocusChange::Application));
    }

    #[test]
    fn visible_nonactivating_overlay_does_not_replace_the_keyboard_snapshot() {
        let original = snapshot(Some(20), Some(11), Some(12));
        // The visible overlay is at the top of the visual stack, but the editor keeps the keyboard.
        let keyboard = read_keyboard_target(20, |attribute| match attribute {
            "AXFocusedWindow" => Some(11),
            "AXFocusedUIElement" => Some(12),
            _ => panic!("queried a non-keyboard signal while the overlay was visible"),
        });
        assert_eq!(original.change(&keyboard, i32::eq), None);
        // Actually focusing the SayItLocally app is a real change.
        let own_app = snapshot(Some(std::process::id() as i32), Some(39), None);
        assert_eq!(original.change(&own_app, i32::eq), Some(FocusChange::Application));
    }

    #[test]
    fn superseded_and_released_tokens_cannot_supply_a_snapshot() {
        let first = begin();
        save(&first, Snapshot::default());
        assert!(target(&first).is_ok());
        let second = begin();
        save(&second, Snapshot::default());
        release(&first);
        save(&first, Snapshot::default());
        assert!(matches!(target(&first), Err(FocusChange::Expired)));
        assert!(target(&second).is_ok());
        release(&second);
        assert!(matches!(target(&second), Err(FocusChange::Expired)));
    }
}
