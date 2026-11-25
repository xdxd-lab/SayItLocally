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

struct SavedTarget<T> {
    token: String,
    snapshot: Option<Arc<Snapshot<T>>>,
}

pub(crate) struct FocusTargets<T>(Mutex<Option<SavedTarget<T>>>);

pub(crate) static TARGET: FocusTargets<AxElement> = FocusTargets::new();

struct FocusReader<P, A> {
    own_pid: i32,
    frontmost_pid: P,
    keyboard_target: A,
}

impl<T> FocusTargets<T> {
    const fn new() -> Self {
        Self(Mutex::new(None))
    }

    pub(crate) fn begin(&self) -> String {
        let token = uuid::Uuid::new_v4().to_string();
        *self.0.lock().unwrap_or_else(|error| error.into_inner()) =
            Some(SavedTarget { token: token.clone(), snapshot: None });
        token
    }

    pub(crate) fn save(&self, token: &str, snapshot: Snapshot<T>) {
        let mut slot = self.0.lock().unwrap_or_else(|error| error.into_inner());
        if let Some(entry) = slot.as_mut().filter(|entry| entry.token == token) {
            entry.snapshot = Some(Arc::new(snapshot));
        }
    }

    pub(crate) fn release(&self, token: &str) {
        let mut slot = self.0.lock().unwrap_or_else(|error| error.into_inner());
        if slot.as_ref().is_some_and(|entry| entry.token == token) {
            *slot = None;
        }
    }

    pub(crate) fn ensure_active(&self, token: &str) -> Result<(), FocusChange> {
        self.target(token).map(|_| ())
    }

    fn target(&self, token: &str) -> Result<Arc<Snapshot<T>>, FocusChange> {
        let slot = self.0.lock().unwrap_or_else(|error| error.into_inner());
        slot.as_ref().filter(|entry| entry.token == token)
            .and_then(|entry| entry.snapshot.clone()).ok_or(FocusChange::Expired)
    }

    fn verify_with(
        &self,
        token: Option<&str>,
        legacy_pid: i32,
        reader: FocusReader<impl Fn() -> Option<i32>, impl Fn(i32) -> Option<Snapshot<T>>>,
        equal: impl Fn(&T, &T) -> bool,
    ) -> Result<(), FocusChange> {
        let original = token.map(|token| self.target(token)).transpose()?;
        let expected_pid = original.as_ref().and_then(|snapshot| snapshot.pid)
            .or_else(|| (legacy_pid > 0).then_some(legacy_pid));
        let mut current = Snapshot { pid: (reader.frontmost_pid)(), ..Snapshot::default() };
        if expected_pid.zip(current.pid).is_some_and(|(first, last)| first != last) {
            return Err(FocusChange::Application);
        }

        if let Some(original) = original {
            if original.window.is_some() || original.element.is_some() {
                if let Some(target) = current.pid.filter(|pid| *pid != reader.own_pid)
                    .and_then(reader.keyboard_target)
                {
                    current = target;
                }
            }
            if let Some(change) = original.change(&current, equal) {
                return Err(change);
            }
            // Foreground can change while AX is answering; use the freshest application signal.
            if expected_pid.zip((reader.frontmost_pid)()).is_some_and(|(first, last)| first != last) {
                return Err(FocusChange::Application);
            }
            if let Some(token) = token {
                // The run may have been abandoned or superseded during the AX call.
                self.ensure_active(token)?;
            }
        }
        Ok(())
    }
}

/// Query the keyboard application's AX objects, never the topmost visual window.
fn read_keyboard_target<T>(pid: i32, read: impl Fn(&str) -> Option<T>) -> Snapshot<T> {
    Snapshot {
        pid: Some(pid),
        window: read("AXFocusedWindow"),
        element: read("AXFocusedUIElement"),
    }
}

pub(crate) fn verify(token: Option<&str>, legacy_pid: i32) -> Result<(), FocusChange> {
    TARGET.verify_with(token, legacy_pid, FocusReader {
        own_pid: std::process::id() as i32,
        frontmost_pid: appkit::frontmost_pid,
        keyboard_target: |pid| {
            let application = AxElement::application(pid)?;
            application.set_timeout(AX_FOCUS_TIMEOUT_SECS);
            Some(read_keyboard_target(pid, |name| application.attr_element(name)))
        },
    }, AxElement::equals)
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
        let targets = FocusTargets::new();
        let first = targets.begin();
        targets.save(&first, snapshot(Some(20), None, None));
        assert!(targets.target(&first).is_ok());
        let second = targets.begin();
        targets.save(&second, snapshot(Some(21), None, None));
        targets.release(&first);
        targets.save(&first, snapshot(Some(22), None, None));
        assert!(matches!(targets.target(&first), Err(FocusChange::Expired)));
        assert_eq!(targets.target(&second).unwrap().pid, Some(21));
        targets.release(&second);
        assert!(matches!(targets.target(&second), Err(FocusChange::Expired)));
    }

    #[test]
    fn verify_with_rejects_same_pid_window_change_and_same_window_element_change() {
        let targets = FocusTargets::new();
        let token = targets.begin();
        targets.save(&token, snapshot(Some(20), Some(11), Some(12)));
        for (window, element, why) in [
            (23, 12, FocusChange::Window),
            (11, 24, FocusChange::Element),
        ] {
            let result = targets.verify_with(Some(&token), 20, FocusReader {
                own_pid: 21,
                frontmost_pid: || Some(20),
                keyboard_target: |pid| Some(snapshot(Some(pid), Some(window), Some(element))),
            }, i32::eq);
            assert_eq!(result, Err(why));
        }
    }

    #[test]
    fn verification_rejects_expired_tokens_and_release_during_ax_read() {
        let targets = FocusTargets::new();
        let token = targets.begin();
        targets.save(&token, snapshot(Some(20), Some(11), Some(12)));
        let expired = targets.verify_with(Some("expired"), 20, FocusReader {
            own_pid: 21,
            frontmost_pid: || panic!("expired token must fail before reading the foreground"),
            keyboard_target: |_| None,
        }, i32::eq);
        assert_eq!(expired, Err(FocusChange::Expired));

        let released = targets.verify_with(Some(&token), 20, FocusReader {
            own_pid: 21,
            frontmost_pid: || Some(20),
            keyboard_target: |_| {
                targets.release(&token);
                Some(snapshot(Some(20), Some(11), Some(12)))
            },
        }, i32::eq);
        assert_eq!(released, Err(FocusChange::Expired));
    }

    #[test]
    fn native_verification_rejects_an_unknown_token_before_any_system_query() {
        assert_eq!(verify(Some("unknown-test-token"), 0), Err(FocusChange::Expired));
    }

    #[test]
    fn verifies_the_own_app_by_pid_only_and_still_blocks_switching_to_it() {
        let targets = FocusTargets::new();
        let token = targets.begin();
        targets.save(&token, snapshot(Some(20), Some(11), Some(12)));
        let same_app = || FocusReader {
            own_pid: 20,
            frontmost_pid: || Some(20),
            keyboard_target: |_| panic!("must not query our own AX attributes"),
        };
        assert_eq!(targets.verify_with(Some(&token), 20, same_app(), i32::eq), Ok(()));
        targets.save(&token, snapshot(Some(21), Some(11), Some(12)));
        assert_eq!(targets.verify_with(Some(&token), 21, same_app(), i32::eq), Err(FocusChange::Application));
    }
}
