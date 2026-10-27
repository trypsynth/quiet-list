//! Which rows of a long list a screen reader is shown, and what changed since it was last told.
//!
//! Only rows near the focused one are in the accessibility tree. Each carries its position, and
//! the list carries its length, so "12 of 50,000" still reads right. While the list has keyboard
//! focus, the focused row is never sent again, even when its text changed: a screen reader reads its
//! focused item again whenever that item changes, which would interrupt the user every time the
//! list changes in the background. The row catches up once focus has moved on, to another row or out of
//! the list, so it's current when the user comes back.

use std::{collections::HashMap, hash::Hash, ops::Range};

/// The list itself. Rows get ids above it.
pub const ROOT: u64 = 1;

/// How many rows either side of the focused one are in the tree, for a screen reader's own
/// navigation (NVDA's object navigation, for example).
pub const REACH: usize = 25;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Row {
	pub id: u64,
	/// Zero-based position in the whole list.
	pub index: usize,
	pub label: String,
}

#[derive(Debug, PartialEq, Eq)]
pub struct Update {
	/// The list's children, in order.
	pub children: Vec<u64>,
	/// Rows that are new or changed.
	pub rows: Vec<Row>,
	/// The focused row, or `ROOT` when the list is empty.
	pub focus: u64,
}

/// The rows to put in the tree for a list of `len` rows with `selected` focused.
pub fn reach(selected: Option<usize>, len: usize) -> Range<usize> {
	let centre = selected.unwrap_or(0).min(len.saturating_sub(1));
	let start = centre.saturating_sub(REACH);
	start..(centre + REACH + 1).min(len)
}

pub struct ListTree<K> {
	ids: HashMap<K, u64>,
	next_id: u64,
	/// What the screen reader was last told about each row in the tree: position and label.
	sent: HashMap<u64, (usize, String)>,
	focus: u64,
}

impl<K> Default for ListTree<K> {
	fn default() -> Self {
		Self { ids: HashMap::new(), next_id: ROOT + 1, sent: HashMap::new(), focus: ROOT }
	}
}

impl<K: Clone + Eq + Hash> ListTree<K> {
	/// Forgets what was sent, so the next update describes every row: for a screen reader that
	/// has just connected.
	pub fn reset(&mut self) {
		self.ids.clear();
		self.sent.clear();
		self.focus = ROOT;
	}

	/// The key of the row with tree id `id`, if it's in the tree.
	pub fn key_of(&self, id: u64) -> Option<&K> {
		self.ids.iter().find_map(|(key, row)| (*row == id).then_some(key))
	}

	/// `rows` are the rows to put in the tree, in order, each with its position and label;
	/// `focus` is the focused row's key, and `has_focus` whether the list has keyboard focus.
	pub fn update(&mut self, rows: Vec<(K, usize, String)>, focus: Option<&K>, has_focus: bool) -> Update {
		let mut ids = HashMap::with_capacity(rows.len());
		for (key, ..) in &rows {
			let id = self.ids.get(key).copied().unwrap_or_else(|| {
				self.next_id += 1;
				self.next_id - 1
			});
			ids.insert(key.clone(), id);
		}
		let focus_id = focus.and_then(|key| ids.get(key)).copied().unwrap_or(ROOT);
		let mut update = Update { children: Vec::with_capacity(rows.len()), rows: Vec::new(), focus: focus_id };
		let mut sent = HashMap::with_capacity(rows.len());
		for (key, index, label) in rows {
			let id = ids[&key];
			update.children.push(id);
			let state = (index, label);
			if let Some(previous) = self.sent.remove(&id) {
				// The row that had focus keeps what it last said, whether it still has focus or
				// focus is leaving it for another row: a change to it either way would be read
				// out. Outside the list nothing reads it, so it's brought up to date.
				if previous == state || (has_focus && id == self.focus) {
					sent.insert(id, previous);
					continue;
				}
			}
			update.rows.push(Row { id, index, label: state.1.clone() });
			sent.insert(id, state);
		}
		self.ids = ids;
		self.sent = sent;
		self.focus = focus_id;
		update
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	fn rows(keys: &[&str], labels: &[&str], start: usize) -> Vec<(String, usize, String)> {
		keys.iter()
			.zip(labels)
			.enumerate()
			.map(|(offset, (key, label))| ((*key).to_owned(), start + offset, (*label).to_owned()))
			.collect()
	}

	fn sent_labels(update: &Update) -> Vec<&str> {
		update.rows.iter().map(|row| row.label.as_str()).collect()
	}

	fn key(name: &str) -> String {
		name.to_owned()
	}

	#[test]
	fn the_first_update_describes_every_row() {
		let mut tree = ListTree::default();
		let update = tree.update(rows(&["a", "b", "c"], &["A", "B", "C"], 0), Some(&key("b")), true);
		assert_eq!(sent_labels(&update), ["A", "B", "C"]);
		assert_eq!(update.children.len(), 3);
		assert_eq!(update.focus, update.children[1]);
		let positions: Vec<usize> = update.rows.iter().map(|row| row.index).collect();
		assert_eq!(positions, [0, 1, 2]);
	}

	#[test]
	fn nothing_is_sent_when_nothing_changed() {
		let mut tree = ListTree::default();
		tree.update(rows(&["a", "b"], &["A", "B"], 0), Some(&key("a")), true);
		let update = tree.update(rows(&["a", "b"], &["A", "B"], 0), Some(&key("a")), true);
		assert_eq!(update.rows, Vec::<Row>::new());
	}

	#[test]
	fn rows_arriving_below_send_only_the_new_rows() {
		let mut tree = ListTree::default();
		tree.update(rows(&["a", "b"], &["A", "B"], 0), Some(&key("a")), true);
		let update = tree.update(rows(&["a", "b", "c"], &["A", "B", "C"], 0), Some(&key("a")), true);
		assert_eq!(sent_labels(&update), ["C"]);
	}

	#[test]
	fn a_focused_row_is_left_alone_when_it_changes_in_the_background() {
		let mut tree = ListTree::default();
		let first = tree.update(rows(&["a", "b"], &["A", "B"], 0), Some(&key("a")), true);
		let update = tree.update(rows(&["a", "b"], &["A read", "B read"], 0), Some(&key("a")), true);
		assert_eq!(sent_labels(&update), ["B read"]);
		assert_eq!(update.focus, first.focus, "focus didn't move");
	}

	#[test]
	fn the_row_focus_leaves_catches_up_on_the_next_update() {
		let mut tree = ListTree::default();
		tree.update(rows(&["a", "b"], &["A", "B"], 0), Some(&key("a")), true);
		tree.update(rows(&["a", "b"], &["A read", "B"], 0), Some(&key("a")), true);
		let moved = tree.update(rows(&["a", "b"], &["A read", "B"], 0), Some(&key("b")), true);
		assert!(moved.rows.is_empty(), "leaving A mustn't change it: {moved:?}");
		let next = tree.update(rows(&["a", "b"], &["A read", "B"], 0), Some(&key("b")), true);
		assert_eq!(sent_labels(&next), ["A read"]);
	}

	#[test]
	fn the_focused_row_catches_up_when_focus_leaves_the_list() {
		let mut tree = ListTree::default();
		tree.update(rows(&["a", "b"], &["A", "B"], 0), Some(&key("a")), true);
		tree.update(rows(&["a", "b"], &["A read", "B"], 0), Some(&key("a")), true);
		let left = tree.update(rows(&["a", "b"], &["A read", "B"], 0), Some(&key("a")), false);
		assert_eq!(sent_labels(&left), ["A read"]);
		// Back in the list, it's current and stays so.
		let back = tree.update(rows(&["a", "b"], &["A read", "B"], 0), Some(&key("a")), true);
		assert!(back.rows.is_empty(), "{back:?}");
	}

	#[test]
	fn rows_change_freely_while_the_list_has_no_focus() {
		let mut tree = ListTree::default();
		tree.update(rows(&["a"], &["A"], 0), Some(&key("a")), false);
		let update = tree.update(rows(&["a"], &["A read"], 0), Some(&key("a")), false);
		assert_eq!(sent_labels(&update), ["A read"]);
	}

	#[test]
	fn a_row_keeps_its_id_when_rows_arrive_above_it() {
		let mut tree = ListTree::default();
		let before = tree.update(rows(&["a", "b"], &["A", "B"], 0), Some(&key("b")), true);
		let after = tree.update(rows(&["new", "a", "b"], &["New", "A", "B"], 0), Some(&key("b")), true);
		assert_eq!(after.focus, before.focus);
		assert_eq!(after.children[2], before.children[1]);
		// A moved down, so it's sent again; the focused B moved too, but is left alone.
		assert_eq!(sent_labels(&after), ["New", "A"]);
	}

	#[test]
	fn rows_that_leave_the_tree_come_back_as_new_rows() {
		let mut tree = ListTree::default();
		let first = tree.update(rows(&["a", "b"], &["A", "B"], 0), Some(&key("a")), true);
		tree.update(rows(&["b", "c"], &["B", "C"], 1), Some(&key("b")), true);
		let back = tree.update(rows(&["a", "b"], &["A", "B"], 0), Some(&key("a")), true);
		assert_ne!(back.children[0], first.children[0]);
		assert_eq!(sent_labels(&back), ["A"]);
	}

	#[test]
	fn ids_lead_back_to_their_rows() {
		let mut tree = ListTree::default();
		let update = tree.update(rows(&["a", "b"], &["A", "B"], 0), Some(&key("a")), true);
		assert_eq!(tree.key_of(update.children[1]), Some(&key("b")));
		assert_eq!(tree.key_of(ROOT), None);
	}

	#[test]
	fn an_empty_list_focuses_itself() {
		let mut tree: ListTree<String> = ListTree::default();
		let update = tree.update(Vec::new(), None, true);
		assert_eq!(update.focus, ROOT);
		assert_eq!(update.children, Vec::<u64>::new());
	}

	#[test]
	fn reset_describes_everything_again() {
		let mut tree = ListTree::default();
		tree.update(rows(&["a"], &["A"], 0), Some(&key("a")), true);
		tree.reset();
		let update = tree.update(rows(&["a"], &["A"], 0), Some(&key("a")), true);
		assert_eq!(sent_labels(&update), ["A"]);
	}

	#[test]
	fn reach_surrounds_the_selection_within_the_list() {
		assert_eq!(reach(None, 0), 0..0);
		assert_eq!(reach(None, 5), 0..5);
		assert_eq!(reach(Some(0), 1000), 0..REACH + 1);
		assert_eq!(reach(Some(500), 1000), 500 - REACH..500 + REACH + 1);
		assert_eq!(reach(Some(999), 1000), 999 - REACH..1000);
	}
}
