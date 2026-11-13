// SPDX-License-Identifier: GPL-3.0-or-later
//! Undo and redo of the conversion settings (Ctrl+Z, Ctrl+Y or Ctrl+Shift+Z).
//!
//! A step is the settings and mode as they are when no mouse button is held, so a drag of a value or a
//! cut click is one step. Opening a file starts a new history.

use super::Desktop;
use cad2bem::conversion::{Mode, Settings};
use eframe::egui;

/// Steps kept for undo.
const DEPTH: usize = 100;

type State = (Settings, Mode);

#[derive(Default)]
pub(super) struct History {
    undo: Vec<State>,
    redo: Vec<State>,
    /// The state of the last step.
    last: Option<State>,
}

impl History {
    /// Forget every step (a new file).
    pub(super) fn clear(&mut self) {
        *self = Self::default();
    }

    pub(super) fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    pub(super) fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    /// Record `now` as a step when it differs from the last one.
    fn track(&mut self, now: &State) {
        match &self.last {
            Some(last) if last == now => {}
            Some(_) => {
                let last = self.last.replace(now.clone()).expect("checked");
                self.undo.push(last);
                if self.undo.len() > DEPTH {
                    self.undo.remove(0);
                }
                self.redo.clear();
            }
            None => self.last = Some(now.clone()),
        }
    }

    /// The state before `now`, keeping `now` for redo.
    fn back(&mut self, now: State) -> Option<State> {
        let state = self.undo.pop()?;
        self.redo.push(now);
        self.last = Some(state.clone());
        Some(state)
    }

    /// The state undone last, keeping `now` for undo.
    fn forward(&mut self, now: State) -> Option<State> {
        let state = self.redo.pop()?;
        self.undo.push(now);
        self.last = Some(state.clone());
        Some(state)
    }
}

impl Desktop {
    /// Ctrl+Z / Ctrl+Y (or Ctrl+Shift+Z), unless a text field has the keyboard. Run before the panels.
    pub(super) fn undo_keys(&mut self, ctx: &egui::Context) {
        if ctx.wants_keyboard_input() {
            return;
        }
        let (undo, redo) = ctx.input_mut(|i| {
            let redo = i.consume_key(
                egui::Modifiers::COMMAND | egui::Modifiers::SHIFT,
                egui::Key::Z,
            ) || i.consume_key(egui::Modifiers::COMMAND, egui::Key::Y);
            let undo = i.consume_key(egui::Modifiers::COMMAND, egui::Key::Z);
            (undo, redo)
        });
        if undo {
            self.undo();
        } else if redo {
            self.redo();
        }
    }

    pub(super) fn undo(&mut self) {
        let now = (self.settings.clone(), self.mode);
        if let Some(state) = self.history.back(now) {
            self.restore(state, "Undone");
        }
    }

    pub(super) fn redo(&mut self) {
        let now = (self.settings.clone(), self.mode);
        if let Some(state) = self.history.forward(now) {
            self.restore(state, "Redone");
        }
    }

    fn restore(&mut self, (settings, mode): State, what: &str) {
        self.settings = settings;
        self.mode = mode;
        self.status = format!("{what} (Ctrl+Z undo, Ctrl+Y redo)");
        self.log.add(what);
    }

    /// Record a step once no mouse button is held. Run after the panels.
    pub(super) fn track_history(&mut self, ctx: &egui::Context) {
        if self.source.is_none() || ctx.input(|i| i.pointer.any_down()) {
            return;
        }
        let same = self.history.last.as_ref();
        if same.is_some_and(|(s, m)| *s == self.settings && *m == self.mode) {
            return; // no clone in the usual frame
        }
        let now = (self.settings.clone(), self.mode);
        self.history.track(&now);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state(size: f64) -> State {
        (
            Settings {
                panel_size: Some(size),
                ..Default::default()
            },
            Mode::LoFree,
        )
    }

    #[test]
    fn undo_and_redo_walk_the_steps_and_a_new_step_drops_the_redo() {
        let mut h = History::default();
        h.track(&state(1.));
        h.track(&state(1.));
        h.track(&state(2.));
        h.track(&state(3.));
        assert_eq!(h.back(state(3.)), Some(state(2.)));
        assert_eq!(h.back(state(2.)), Some(state(1.)));
        assert_eq!(h.back(state(1.)), None);
        assert_eq!(h.forward(state(1.)), Some(state(2.)));
        // Restored states are not new steps.
        h.track(&state(2.));
        assert!(h.can_redo());
        h.track(&state(5.));
        assert!(!h.can_redo());
        assert_eq!(h.back(state(5.)), Some(state(2.)));
    }
}
