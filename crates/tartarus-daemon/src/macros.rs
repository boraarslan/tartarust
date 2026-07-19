//! Macro playback model.
//!
//! A macro is an ordered list of [`tartarus_ipc::model::MacroStep`]s. This
//! module lowers those into a flat list of runtime [`MacroAction`]s (emit-events
//! or sleep) that the output thread replays in order. Lowering is pure and
//! unit-tested here; the actual sleeping/emitting happens in [`crate::output`].

use std::time::Duration;

use evdev::{InputEvent, KeyEvent};

use tartarus_ipc::model::MacroStep;

/// A single runtime action during macro playback.
#[derive(Debug, Clone, PartialEq)]
pub enum MacroAction {
    /// Emit this batch of input events (uinput appends one `SYN_REPORT`).
    Emit(Vec<InputEvent>),
    /// Sleep this long before the next action.
    Delay(Duration),
}

/// Lower a config macro (list of [`MacroStep`]) into runtime [`MacroAction`]s.
///
/// `Press`/`Release` take an evdev key name string; an unknown name is skipped
/// with a warning (names are validated when binds are set, so this is rare).
pub fn lower(steps: &[MacroStep]) -> Vec<MacroAction> {
    let mut out = Vec::with_capacity(steps.len());
    for step in steps {
        match step {
            MacroStep::Press { key } => {
                if let Some(code) = parse(key) {
                    out.push(MacroAction::Emit(vec![*KeyEvent::new(code, 1)]));
                }
            }
            MacroStep::Release { key } => {
                if let Some(code) = parse(key) {
                    out.push(MacroAction::Emit(vec![*KeyEvent::new(code, 0)]));
                }
            }
            MacroStep::Delay { ms } => {
                out.push(MacroAction::Delay(Duration::from_millis(*ms)));
            }
        }
    }
    out
}

fn parse(name: &str) -> Option<evdev::KeyCode> {
    let code = crate::engine::parse_key(name);
    if code.is_none() {
        tracing::warn!(%name, "macro step references unknown key name; skipping");
    }
    code
}

#[cfg(test)]
mod tests {
    use super::*;
    use evdev::{EventType, KeyCode};

    #[test]
    fn press_release_lower_to_key_events() {
        let steps = vec![
            MacroStep::Press {
                key: "KEY_A".into(),
            },
            MacroStep::Release {
                key: "KEY_A".into(),
            },
        ];
        let actions = lower(&steps);
        assert_eq!(actions.len(), 2);
        if let MacroAction::Emit(evs) = &actions[0] {
            assert_eq!(evs[0].event_type(), EventType::KEY);
            assert_eq!(evs[0].code(), KeyCode::KEY_A.0);
            assert_eq!(evs[0].value(), 1);
        } else {
            panic!("expected emit");
        }
        if let MacroAction::Emit(evs) = &actions[1] {
            assert_eq!(evs[0].value(), 0);
        } else {
            panic!("expected emit");
        }
    }

    #[test]
    fn delays_preserved_and_summed() {
        let steps = vec![
            MacroStep::Press {
                key: "KEY_H".into(),
            },
            MacroStep::Delay { ms: 50 },
            MacroStep::Release {
                key: "KEY_H".into(),
            },
            MacroStep::Delay { ms: 30 },
        ];
        let actions = lower(&steps);
        assert_eq!(actions.len(), 4);
        let delays: Vec<_> = actions
            .iter()
            .filter_map(|a| match a {
                MacroAction::Delay(d) => Some(*d),
                MacroAction::Emit(_) => None,
            })
            .collect();
        assert_eq!(
            delays,
            vec![Duration::from_millis(50), Duration::from_millis(30)]
        );
    }

    #[test]
    fn unknown_key_name_skipped() {
        let steps = vec![
            MacroStep::Press {
                key: "KEY_NOPE".into(),
            },
            MacroStep::Press {
                key: "KEY_A".into(),
            },
        ];
        let actions = lower(&steps);
        // Only the valid one survives.
        assert_eq!(actions.len(), 1);
    }
}
