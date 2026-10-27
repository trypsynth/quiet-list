//! The model behind the list: the rows' keys in order, and their labels. Labels are either given
//! with the rows, or read a page at a time when a row is first shown, so a list of any length
//! costs the same to show.

use std::{
	collections::{HashMap, HashSet},
	hash::Hash,
};

/// Rows labelled together, starting a little above the one asked for, so moving through the
/// list in either direction reads ahead rather than one row at a time.
pub const PAGE: usize = 200;
/// Labels kept before the cache starts over; enough for several screens either way.
pub const CACHE_LIMIT: usize = 5_000;

pub struct LazyRows<K> {
	keys: Vec<K>,
	labels: HashMap<K, String>,
	/// Every row's label was given with the rows, so none are read and none are forgotten.
	complete: bool,
}

impl<K> Default for LazyRows<K> {
	fn default() -> Self {
		Self { keys: Vec::new(), labels: HashMap::new(), complete: false }
	}
}

impl<K: Clone + Eq + Hash> LazyRows<K> {
	pub const fn len(&self) -> usize {
		self.keys.len()
	}

	pub const fn is_empty(&self) -> bool {
		self.keys.is_empty()
	}

	pub fn keys(&self) -> &[K] {
		&self.keys
	}

	pub fn key(&self, index: usize) -> Option<&K> {
		self.keys.get(index)
	}

	pub fn position(&self, key: &K) -> Option<usize> {
		self.keys.iter().position(|candidate| candidate == key)
	}

	/// Replaces the rows, to be labelled as they're shown. Labels are forgotten, since any row
	/// may have changed. A key that's already in the list is left out.
	pub fn replace(&mut self, keys: Vec<K>) {
		self.keys = unique(keys);
		self.labels.clear();
		self.complete = false;
	}

	/// Replaces the rows with ones that come with their labels. A key that's already in the list
	/// is left out.
	pub fn replace_labelled(&mut self, rows: Vec<(K, String)>) {
		self.keys.clear();
		self.labels.clear();
		for (key, label) in rows {
			if !self.labels.contains_key(&key) {
				self.keys.push(key.clone());
				self.labels.insert(key, label);
			}
		}
		self.complete = true;
	}

	/// The label of the row at `index`, asking `load` for its page if it hasn't been read.
	/// `load` gets the keys to label and returns a label for each it can.
	pub fn label(&mut self, index: usize, load: impl FnOnce(&[K]) -> HashMap<K, String>) -> String {
		let Some(key) = self.keys.get(index) else {
			return String::new();
		};
		if let Some(label) = self.labels.get(key) {
			return label.clone();
		}
		if self.complete {
			return String::new();
		}
		if self.labels.len() >= CACHE_LIMIT {
			self.labels.clear();
		}
		let start = index.saturating_sub(PAGE / 4);
		let end = (start + PAGE).min(self.keys.len());
		let wanted: Vec<K> =
			self.keys[start..end].iter().filter(|key| !self.labels.contains_key(*key)).cloned().collect();
		self.labels.extend(load(&wanted));
		self.labels.get(&self.keys[index]).cloned().unwrap_or_default()
	}
}

/// `keys` without the ones already seen earlier in it. Only a list with repeats is copied.
fn unique<K: Clone + Eq + Hash>(mut keys: Vec<K>) -> Vec<K> {
	let repeats = {
		let mut seen = HashSet::with_capacity(keys.len());
		!keys.iter().all(|key| seen.insert(key))
	};
	if repeats {
		let mut seen = HashSet::with_capacity(keys.len());
		keys.retain(|key| seen.insert(key.clone()));
	}
	keys
}

/// Where the selection should go after a list changed: the same row if it survived, else the
/// row that took its place (the next one), else the last row.
pub fn reselect<K: Eq>(previous: Option<(&K, usize)>, new_keys: &[K]) -> Option<usize> {
	let (key, old_index) = previous?;
	if new_keys.is_empty() {
		return None;
	}
	Some(new_keys.iter().position(|candidate| candidate == key).unwrap_or_else(|| old_index.min(new_keys.len() - 1)))
}

#[cfg(test)]
mod tests {
	use std::ops::Range;

	use super::*;

	fn rows(count: usize) -> LazyRows<usize> {
		let mut rows = LazyRows::default();
		rows.replace((0..count).collect());
		rows
	}

	fn numbered(keys: &[usize]) -> HashMap<usize, String> {
		keys.iter().map(|key| (*key, format!("row {key}"))).collect()
	}

	#[test]
	fn a_row_is_read_with_its_page_and_the_page_is_reused() {
		let mut rows = rows(1000);
		let mut asked = Vec::new();
		assert_eq!(
			rows.label(500, |keys| {
				asked = keys.to_vec();
				numbered(keys)
			}),
			"row 500"
		);
		assert_eq!(asked.first(), Some(&(500 - PAGE / 4)));
		assert_eq!(asked.len(), PAGE);
		for index in [451, 499, 501, 640] {
			let label = rows.label(index, |_| panic!("row {index} should already be read"));
			assert_eq!(label, format!("row {index}"));
		}
	}

	#[test]
	fn pages_stop_at_the_ends_and_only_read_missing_rows() {
		let mut rows = rows(30);
		rows.label(0, |keys| {
			assert_eq!(keys, (0..30).collect::<Vec<_>>());
			numbered(keys)
		});
		let mut rows = rows_with_labels(400, 0..PAGE);
		rows.label(PAGE, |keys| {
			assert!(keys.iter().all(|key| *key >= PAGE), "{keys:?}");
			numbered(keys)
		});
	}

	fn rows_with_labels(count: usize, read: Range<usize>) -> LazyRows<usize> {
		let mut rows = rows(count);
		let keys: Vec<usize> = read.collect();
		rows.labels.extend(numbered(&keys));
		rows
	}

	#[test]
	fn replacing_rows_forgets_labels_and_unknown_rows_are_blank() {
		let mut rows = rows(10);
		rows.label(3, numbered);
		rows.replace(vec![3, 4]);
		assert_eq!(rows.label(0, |_| HashMap::new()), "");
		assert_eq!(rows.label(1, numbered), "row 4");
		assert_eq!(rows.label(2, |_| panic!("no such row")), "");
	}

	#[test]
	fn the_cache_starts_over_instead_of_growing_without_end() {
		let mut rows = rows_with_labels(CACHE_LIMIT * 2, 0..CACHE_LIMIT);
		rows.label(CACHE_LIMIT + PAGE, numbered);
		assert!(rows.labels.len() <= PAGE);
	}

	#[test]
	fn labelled_rows_are_never_read_or_forgotten() {
		let mut rows = LazyRows::default();
		let labelled: Vec<(usize, String)> = (0..CACHE_LIMIT * 2).map(|key| (key, format!("row {key}"))).collect();
		rows.replace_labelled(labelled);
		for index in [0, CACHE_LIMIT, CACHE_LIMIT * 2 - 1] {
			assert_eq!(rows.label(index, |_| panic!("labels were given")), format!("row {index}"));
		}
		rows.replace(vec![1]);
		assert_eq!(rows.label(0, numbered), "row 1", "replacing goes back to reading labels");
	}

	#[test]
	fn repeated_keys_keep_their_first_row() {
		let mut rows = LazyRows::default();
		rows.replace(vec![1, 2, 1, 3, 2]);
		assert_eq!(rows.keys(), [1, 2, 3]);
		rows.replace_labelled(vec![(1, "one".to_owned()), (2, "two".to_owned()), (1, "again".to_owned())]);
		assert_eq!(rows.keys(), [1, 2]);
		assert_eq!(rows.label(0, |_| HashMap::new()), "one");
	}

	#[test]
	fn selection_follows_the_row_or_falls_to_the_next() {
		assert_eq!(reselect(Some((&2, 1)), &[9, 1, 2]), Some(2));
		// Row 2 was deleted: the row that moved into its slot is selected.
		assert_eq!(reselect(Some((&2, 1)), &[1, 3]), Some(1));
		// The last row was deleted: select the new last row.
		assert_eq!(reselect(Some((&3, 2)), &[1, 2]), Some(1));
		assert_eq!(reselect(Some((&3, 0)), &[] as &[i32]), None);
		assert_eq!(reselect::<i32>(None, &[1]), None);
	}
}
