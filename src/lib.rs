//! A wxDragon list of any length whose rows can change without the screen reader reading them out
//! again.
//!
//! Rows are identified by key, and the selection follows its row through every update, so rows
//! arriving, leaving or changing in the background neither move the user nor make the screen
//! reader repeat itself. Labels can come with the rows ([`QuietList::update_labelled`]), or be
//! asked for a page at a time as rows are shown or read ([`QuietList::with_labeler`]), so a list of
//! 50,000 rows shows as quickly as one of 50.
//!
//! - Windows: the list is drawn by this crate and described to screen readers through AccessKit,
//!   with only the rows near the selection in the tree. The native list view can't be used: every
//!   change to its row count recreates its accessible children and sends focus to the selected row
//!   again, so NVDA reads that row out each time.
//! - Elsewhere: a native virtual list view, which asks for a row's text only when it's drawn or
//!   read.
//!
//! The list is a single control with no label of its own. Create its label first, with the same
//! parent, so screen readers on Windows find it, and pass the spoken name to the constructor too.

#[cfg_attr(windows, expect(dead_code, reason = "compiled on Windows too, so it can't silently stop building"))]
mod native;
mod rows;
#[cfg(any(windows, test))]
mod tree;
#[cfg(windows)]
mod windows;

#[cfg(not(windows))]
pub use native::QuietList;
#[cfg(windows)]
pub use windows::QuietList;
use wxdragon::prelude::*;

/// Where the selection goes when the rows are replaced.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Select<K> {
	/// Stay on the selected row wherever it moved, or move to the row that took its place if it's
	/// gone. A list with nothing selected starts at its start. For background updates, which mustn't
	/// move the user.
	Keep,
	/// The list's first row, or its last if it starts at its end
	/// ([`QuietList::set_start_at_end`]). For a different list altogether.
	Start,
	/// The row with this key, or the start if it isn't in the list.
	Key(K),
}

/// A move through the list, as a navigation key makes it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
	Up,
	Down,
	PageUp,
	PageDown,
	Home,
	End,
}

impl Step {
	/// The step a key code makes, if it's an arrow, Page Up, Page Down, Home or End key.
	#[must_use]
	pub const fn from_key(code: i32) -> Option<Self> {
		Some(match code {
			WXK_UP | WXK_NUMPAD_UP => Self::Up,
			WXK_DOWN | WXK_NUMPAD_DOWN => Self::Down,
			WXK_PAGEUP | WXK_NUMPAD_PAGEUP => Self::PageUp,
			WXK_PAGEDOWN | WXK_NUMPAD_PAGEDOWN => Self::PageDown,
			WXK_HOME | WXK_NUMPAD_HOME => Self::Home,
			WXK_END | WXK_NUMPAD_END => Self::End,
			_ => return None,
		})
	}

	/// The row this step lands on from `current` in a list of `len` rows (at least one), `page`
	/// rows to a screen.
	const fn target(self, current: usize, len: usize, page: usize) -> usize {
		let last = len.saturating_sub(1);
		match self {
			Self::Up => current.saturating_sub(1),
			Self::Down => min(current + 1, last),
			Self::PageUp => current.saturating_sub(page),
			Self::PageDown => min(current + page, last),
			Self::Home => 0,
			Self::End => last,
		}
	}
}

const fn min(a: usize, b: usize) -> usize {
	if a < b { a } else { b }
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn steps_stop_at_the_ends() {
		assert_eq!(Step::Up.target(0, 10, 4), 0);
		assert_eq!(Step::Down.target(9, 10, 4), 9);
		assert_eq!(Step::Down.target(3, 10, 4), 4);
		assert_eq!(Step::PageUp.target(2, 10, 4), 0);
		assert_eq!(Step::PageDown.target(3, 10, 4), 7);
		assert_eq!(Step::PageDown.target(8, 10, 4), 9);
		assert_eq!(Step::Home.target(5, 10, 4), 0);
		assert_eq!(Step::End.target(5, 10, 4), 9);
	}

	#[test]
	fn navigation_keys_make_steps() {
		assert_eq!(Step::from_key(WXK_NUMPAD_END), Some(Step::End));
		assert_eq!(Step::from_key(WXK_PAGEDOWN), Some(Step::PageDown));
		assert_eq!(Step::from_key(WXK_TAB), None);
	}
}
