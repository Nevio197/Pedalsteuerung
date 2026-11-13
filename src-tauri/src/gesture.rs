use crate::model::{Action, ActionPhase, Bindings};
use std::time::{Duration, Instant};

#[derive(Debug, PartialEq)]
pub struct Dispatch {
    pub position: usize,
    pub action: Action,
    pub phase: ActionPhase,
}

#[derive(Default)]
struct Button {
    pressed_at: Option<Instant>,
    first_tap_at: Option<Instant>,
    hold_fired: bool,
}

#[derive(Default)]
pub struct Gestures {
    buttons: [Button; 3],
}

impl Gestures {
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    pub fn tick(
        &mut self,
        now: Instant,
        bindings: &[Bindings; 3],
        hold: Duration,
        double: Duration,
    ) -> Vec<Dispatch> {
        let mut pending = Vec::new();
        for (position, button) in self.buttons.iter_mut().enumerate() {
            button.tick(
                now,
                position,
                &bindings[position],
                hold,
                double,
                &mut pending,
            );
        }
        pending
    }

    pub fn input(
        &mut self,
        position: usize,
        pressed: bool,
        now: Instant,
        binding: &Bindings,
        hold: Duration,
        double: Duration,
    ) -> Vec<Dispatch> {
        let button = &mut self.buttons[position];
        let mut pending = Vec::new();
        button.tick(now, position, binding, hold, double, &mut pending);

        if pressed {
            if button.pressed_at.is_none() {
                button.pressed_at = Some(now);
                if binding.hold.is_none() && binding.double_tap.is_none() {
                    push(
                        &mut pending,
                        position,
                        binding.tap.as_ref(),
                        ActionPhase::Pulse,
                    );
                }
            }
            return pending;
        }

        if button.pressed_at.take().is_none() {
            return pending;
        }
        if std::mem::take(&mut button.hold_fired) {
            if let Some(action @ Action::DiscordHold { .. }) = &binding.hold {
                push(&mut pending, position, Some(action), ActionPhase::Release);
            }
        } else if binding.double_tap.is_some() {
            if button.first_tap_at.take().is_some() {
                push(
                    &mut pending,
                    position,
                    binding.double_tap.as_ref(),
                    ActionPhase::Pulse,
                );
            } else {
                button.first_tap_at = Some(now);
            }
        } else if binding.hold.is_some() {
            push(
                &mut pending,
                position,
                binding.tap.as_ref(),
                ActionPhase::Pulse,
            );
        }
        pending
    }
}

impl Button {
    fn tick(
        &mut self,
        now: Instant,
        position: usize,
        binding: &Bindings,
        hold: Duration,
        double: Duration,
        pending: &mut Vec<Dispatch>,
    ) {
        if let Some(pressed_at) = self.pressed_at {
            // A second press starts inside the double window, but must still be a short tap.
            if self.first_tap_at.is_some()
                && now.saturating_duration_since(pressed_at) >= hold
                && binding.hold.is_none()
            {
                self.first_tap_at = None;
                self.hold_fired = true;
                push(pending, position, binding.tap.as_ref(), ActionPhase::Pulse);
            }
            if !self.hold_fired && now.saturating_duration_since(pressed_at) >= hold {
                if let Some(action) = &binding.hold {
                    if self.first_tap_at.take().is_some() {
                        push(pending, position, binding.tap.as_ref(), ActionPhase::Pulse);
                    }
                    self.hold_fired = true;
                    let phase = if matches!(action, Action::DiscordHold { .. }) {
                        ActionPhase::Press
                    } else {
                        ActionPhase::Pulse
                    };
                    push(pending, position, Some(action), phase);
                }
            }
        } else if self
            .first_tap_at
            .is_some_and(|at| now.saturating_duration_since(at) >= double)
        {
            self.first_tap_at = None;
            push(pending, position, binding.tap.as_ref(), ActionPhase::Pulse);
        }
    }
}

fn push(pending: &mut Vec<Dispatch>, position: usize, action: Option<&Action>, phase: ActionPhase) {
    if let Some(action) = action {
        pending.push(Dispatch {
            position,
            action: action.clone(),
            phase,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const HOLD: Duration = Duration::from_millis(450);
    const DOUBLE: Duration = Duration::from_millis(280);

    fn shortcut(shortcut: &str) -> Option<Action> {
        Some(Action::Shortcut {
            shortcut: shortcut.into(),
        })
    }

    struct Session {
        origin: Instant,
        gestures: Gestures,
        bindings: [Bindings; 3],
    }

    impl Session {
        fn new(binding: Bindings) -> Self {
            Self {
                origin: Instant::now(),
                gestures: Gestures::default(),
                bindings: [binding, Bindings::default(), Bindings::default()],
            }
        }

        fn press(&mut self, at: u64) -> Vec<Dispatch> {
            self.event(at, true)
        }

        fn release(&mut self, at: u64) -> Vec<Dispatch> {
            self.event(at, false)
        }

        fn event(&mut self, at: u64, pressed: bool) -> Vec<Dispatch> {
            self.gestures.input(
                0,
                pressed,
                self.origin + Duration::from_millis(at),
                &self.bindings[0],
                HOLD,
                DOUBLE,
            )
        }

        fn tick(&mut self, at: u64) -> Vec<Dispatch> {
            self.gestures.tick(
                self.origin + Duration::from_millis(at),
                &self.bindings,
                HOLD,
                DOUBLE,
            )
        }
    }

    #[test]
    fn tap_only_fires_on_press_and_ignores_repeat() {
        let mut s = Session::new(Bindings {
            tap: shortcut("F9"),
            ..Bindings::default()
        });
        assert_eq!(s.press(0).len(), 1);
        assert!(s.press(5).is_empty());
        assert!(s.tick(800).is_empty());
        assert!(s.release(801).is_empty());
    }

    #[test]
    fn double_tap_supersedes_single_within_window() {
        let mut s = Session::new(Bindings {
            tap: shortcut("F9"),
            double_tap: shortcut("F10"),
            hold: None,
        });
        assert!(s.press(0).is_empty());
        assert!(s.release(50).is_empty());
        assert!(s.tick(329).is_empty());
        assert!(s.press(329).is_empty());
        assert_eq!(s.release(370)[0].action, shortcut("F10").unwrap());
        assert!(s.tick(800).is_empty());
        s.press(900);
        s.release(930);
        assert_eq!(s.tick(1_210)[0].action, shortcut("F9").unwrap());
    }

    #[test]
    fn hold_has_one_press_and_release_and_no_tap() {
        let mut s = Session::new(Bindings {
            tap: shortcut("F9"),
            hold: Some(Action::DiscordHold {
                shortcut: "F10".into(),
            }),
            double_tap: shortcut("F11"),
        });
        s.press(0);
        assert!(s.tick(449).is_empty());
        assert_eq!(s.tick(450)[0].phase, ActionPhase::Press);
        assert!(s.tick(500).is_empty());
        assert_eq!(s.release(501)[0].phase, ActionPhase::Release);
        assert!(s.tick(900).is_empty());
    }

    #[test]
    fn overdue_hold_is_recognized_even_if_release_arrives_before_timer_tick() {
        let mut s = Session::new(Bindings {
            hold: Some(Action::DiscordHold {
                shortcut: "F10".into(),
            }),
            ..Bindings::default()
        });
        s.press(0);
        let phases: Vec<_> = s
            .release(480)
            .into_iter()
            .map(|event| event.phase)
            .collect();
        assert_eq!(phases, [ActionPhase::Press, ActionPhase::Release]);
    }

    #[test]
    fn reset_discards_waiting_tap_and_stale_release() {
        let mut s = Session::new(Bindings {
            tap: shortcut("F9"),
            double_tap: shortcut("F10"),
            hold: None,
        });
        s.press(0);
        s.release(50);
        s.gestures.reset();
        assert!(s.tick(500).is_empty());
        assert!(s.release(510).is_empty());
    }

    #[test]
    fn held_second_press_is_not_a_double_tap_without_a_hold_assignment() {
        let mut s = Session::new(Bindings {
            tap: shortcut("F9"),
            double_tap: shortcut("F10"),
            hold: None,
        });
        s.press(0);
        s.release(50);
        s.press(300);
        assert!(s.tick(749).is_empty());
        assert_eq!(s.tick(750)[0].action, shortcut("F9").unwrap());
        assert!(s.release(900).is_empty());
    }
}
