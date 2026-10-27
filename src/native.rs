//! The list as a native virtual list view: the system asks for a row's text only when the row is
//! drawn or read by a screen reader. Not used on Windows, where every change to the row count
//! makes the list view re-announce its focused row (see `windows.rs`).

use std::{
	cell::{Cell, RefCell},
	collections::HashMap,
	hash::Hash,
	rc::Rc,
};

use wxdragon::{ffi::wxd_Window_t, prelude::*};

use crate::{
	Select, Step,
	rows::{self, LazyRows},
};

/// Rows a Page Up or Page Down moves through when asked for with [`QuietList::step`]. The list
/// view handles the keys itself, a screen at a time.
const PAGE: usize = 20;

type Labeler<K> = Box<dyn Fn(&[K]) -> HashMap<K, String>>;
type Callback = RefCell<Option<Rc<dyn Fn()>>>;
type KeyHook = RefCell<Option<Rc<dyn Fn(&WindowEventData) -> bool>>>;

struct Model<K> {
	rows: LazyRows<K>,
	labeler: Option<Labeler<K>>,
}

impl<K: Clone + Eq + Hash> Model<K> {
	fn label(&mut self, index: usize) -> String {
		let Self { rows, labeler } = self;
		rows.label(index, |keys| labeler.as_ref().map(|labeler| labeler(keys)).unwrap_or_default())
	}
}

#[derive(Default)]
struct Callbacks {
	/// The list itself is changing the selection, which the selection callback isn't told of.
	changing: Cell<bool>,
	selection_changed: Callback,
	context_menu: Callback,
	space: Callback,
	key_hook: KeyHook,
}

fn run(callback: &Callback) {
	let callback = callback.borrow().clone();
	if let Some(callback) = callback {
		callback();
	}
}

/// A list of rows identified by keys of type `K`.
pub struct QuietList<K> {
	control: ListCtrl,
	model: Rc<RefCell<Model<K>>>,
	callbacks: Rc<Callbacks>,
	/// Whether wx accepted the text callback. Without it every row would be blank.
	ready: bool,
	start_at_end: Cell<bool>,
}

impl<K: 'static> WxWidget for QuietList<K> {
	fn handle_ptr(&self) -> *mut wxd_Window_t {
		self.control.handle_ptr()
	}
}

impl<K: Clone + Eq + Hash + 'static> QuietList<K> {
	/// A list whose rows come with their labels ([`update_labelled`](Self::update_labelled)).
	/// `name` is what screen readers call it.
	pub fn new(parent: &impl WxWidget, name: &str) -> Self {
		Self::build(parent, name, None)
	}

	/// A list that asks for labels as rows are shown or read. `labeler` gets keys to label and
	/// returns a label for each; it's asked a page at a time. `name` is what screen readers call
	/// the list.
	pub fn with_labeler(
		parent: &impl WxWidget,
		name: &str,
		labeler: impl Fn(&[K]) -> HashMap<K, String> + 'static,
	) -> Self {
		Self::build(parent, name, Some(Box::new(labeler)))
	}

	fn build(parent: &impl WxWidget, name: &str, labeler: Option<Labeler<K>>) -> Self {
		// One column and no header, so each row reads as a single line.
		let control = ListCtrl::builder(parent)
			.with_style(
				ListCtrlStyle::Report | ListCtrlStyle::Virtual | ListCtrlStyle::SingleSel | ListCtrlStyle::NoHeader,
			)
			.build();
		control.set_accessibility_label(name);
		control.insert_column(0, "", ListColumnFormat::Left, -1);
		control.bind_internal(EventType::SIZE, move |event| {
			control.set_column_width(0, control.get_client_size().width);
			event.skip(true);
		});
		let model = Rc::new(RefCell::new(Model { rows: LazyRows::default(), labeler }));
		let shown = model.clone();
		// wx asks for text from inside `set_item_count`, while an update may still hold the
		// model; a row left blank then is drawn again a moment later.
		let ready = control.set_virtual_text_callback(move |item, _| {
			shown
				.try_borrow_mut()
				.map(|mut model| model.label(usize::try_from(item).unwrap_or(usize::MAX)))
				.unwrap_or_default()
		});
		let callbacks: Rc<Callbacks> = Rc::default();
		let list = Self { control, model, callbacks, ready, start_at_end: Cell::new(false) };
		list.bind();
		list
	}

	fn bind(&self) {
		let callbacks = self.callbacks.clone();
		let changed = move || {
			if !callbacks.changing.get() {
				run(&callbacks.selection_changed);
			}
		};
		let changed = Rc::new(changed);
		let selected = changed.clone();
		self.control.on_item_selected(move |_| selected());
		self.control.on_item_deselected(move |_| changed());
		let callbacks = self.callbacks.clone();
		self.control.on_item_right_click(move |_| run(&callbacks.context_menu));
		let callbacks = self.callbacks.clone();
		self.control.bind_internal(EventType::KEY_DOWN, move |event| {
			let event = WindowEventData::new(event);
			let WindowEventData::Keyboard(key) = &event else {
				return;
			};
			let hook = callbacks.key_hook.borrow().clone();
			if hook.is_some_and(|hook| hook(&event)) {
				event.skip(false);
				return;
			}
			let code = key.get_key_code().unwrap_or(0);
			let (control, alt, shift) = (key.control_down(), key.alt_down(), key.shift_down());
			if code == WXK_WINDOWS_MENU || (code == WXK_F10 && shift && !control && !alt) {
				event.skip(false);
				run(&callbacks.context_menu);
				return;
			}
			if code == WXK_SPACE && !control && !alt && !shift {
				run(&callbacks.space);
			}
			event.skip(true);
		});
	}

	/// Whether the list was created and can show rows.
	#[must_use]
	pub const fn ready(&self) -> bool {
		self.ready
	}

	/// Shows `menu` at the list.
	pub fn popup_menu(&self, menu: &mut Menu) {
		self.control.popup_menu(menu, None);
	}

	/// Runs `changed` whenever the selected row changes, or the selection is cleared, except
	/// through [`select`](Self::select).
	pub fn on_selection_changed(&self, changed: impl Fn() + 'static) {
		*self.callbacks.selection_changed.borrow_mut() = Some(Rc::new(changed));
	}

	/// Runs `activate` when a row is double-clicked. The list view also reports Enter and Space
	/// as activating a row on Windows: Enter is left to the app, which usually has it on a menu
	/// item already, and Space has [`on_space`](Self::on_space), so those are ignored there.
	pub fn on_activate(&self, activate: impl Fn() + 'static) {
		self.control.on_item_activated(move |_| {
			if !activated_by_keyboard() {
				activate();
			}
		});
	}

	/// Runs `show` when a row is right-clicked, or the Applications key or Shift+F10 is pressed
	/// in the list. Show the menu with [`popup_menu`](Self::popup_menu).
	pub fn on_context_menu(&self, show: impl Fn() + 'static) {
		*self.callbacks.context_menu.borrow_mut() = Some(Rc::new(show));
	}

	/// Runs `pressed` when Space is pressed on its own in the list.
	pub fn on_space(&self, pressed: impl Fn() + 'static) {
		*self.callbacks.space.borrow_mut() = Some(Rc::new(pressed));
	}

	/// Shows `hook` every key pressed in the list before the list acts on it. It returns whether
	/// it handled the key, in which case the list leaves the key alone, and no character is
	/// typed for it.
	pub fn on_key_down(&self, hook: impl Fn(&WindowEventData) -> bool + 'static) {
		*self.callbacks.key_hook.borrow_mut() = Some(Rc::new(hook));
	}

	/// The selected row's position in the list.
	#[must_use]
	pub fn selected_index(&self) -> Option<usize> {
		usize::try_from(self.control.get_first_selected_item()).ok()
	}

	/// The selected row's key.
	#[must_use]
	pub fn selected(&self) -> Option<K> {
		let index = self.selected_index()?;
		self.model.borrow().rows.key(index).cloned()
	}

	/// The selected row's label, as the list shows it.
	#[must_use]
	pub fn selected_label(&self) -> Option<String> {
		let index = self.selected_index()?;
		Some(self.model.borrow_mut().label(index))
	}

	#[must_use]
	pub fn len(&self) -> usize {
		self.model.borrow().rows.len()
	}

	#[must_use]
	pub fn is_empty(&self) -> bool {
		self.model.borrow().rows.is_empty()
	}

	/// Moves the selection to `index`, or clears it, without telling the selection callback.
	fn set_selected(&self, index: Option<usize>) {
		let state = ListItemState::Selected | ListItemState::Focused;
		self.callbacks.changing.set(true);
		match index {
			Some(index) => {
				let index = i64::try_from(index).unwrap_or(i64::MAX);
				self.control.set_item_state(index, state, state);
				self.control.ensure_visible(index);
			}
			None => {
				if let Some(current) = self.selected_index() {
					self.control.set_item_state(i64::try_from(current).unwrap_or(i64::MAX), ListItemState::None, state);
				}
			}
		}
		self.callbacks.changing.set(false);
	}

	/// Whether a list that shows something new starts on its last row instead of its first.
	pub fn set_start_at_end(&self, at_end: bool) {
		self.start_at_end.set(at_end);
	}

	/// Replaces the rows, to be labelled by the labeler as they're shown. A key that's already in
	/// the list is left out. The selection callback runs if the selected row changed.
	pub fn update(&self, keys: Vec<K>, select: Select<K>) {
		self.replace(|rows| rows.replace(keys), select);
	}

	/// Replaces the rows with ones that come with their labels. A key that's already in the list
	/// is left out. The selection callback runs if the selected row changed.
	pub fn update_labelled(&self, rows: Vec<(K, String)>, select: Select<K>) {
		self.replace(|current| current.replace_labelled(rows), select);
	}

	fn replace(&self, replace: impl FnOnce(&mut LazyRows<K>), select: Select<K>) {
		let fresh = select == Select::Start;
		let previous = self
			.selected_index()
			.and_then(|index| self.model.borrow().rows.key(index).cloned().map(|key| (key, index)));
		let (count, target, key_changed) = {
			let mut model = self.model.borrow_mut();
			let rows = &mut model.rows;
			replace(rows);
			let last = rows.len().checked_sub(1);
			let start = if self.start_at_end.get() { last } else { last.map(|_| 0) };
			let target = match select {
				Select::Start => start,
				Select::Key(key) => rows.position(&key).or(start),
				Select::Keep if previous.is_none() => start,
				Select::Keep => rows::reselect(previous.as_ref().map(|(key, index)| (key, *index)), rows.keys()),
			};
			let key_changed =
				fresh || previous.as_ref().map(|(key, _)| key) != target.and_then(|index| rows.key(index));
			(rows.len(), target, key_changed)
		};
		let count = i64::try_from(count).unwrap_or(i64::MAX);
		self.callbacks.changing.set(true);
		self.control.set_item_count(count);
		if count > 0 {
			self.control.refresh_items(0, count - 1);
		}
		self.callbacks.changing.set(false);
		// Moving the selection is announced, so it only happens when the row actually moved.
		let index_moved = previous.as_ref().map(|(_, index)| *index) != target;
		if let Some(target) = target
			&& (fresh || index_moved || self.selected_index() != Some(target))
		{
			self.set_selected(Some(target));
		}
		if key_changed {
			run(&self.callbacks.selection_changed);
		}
	}

	/// Selects the row with `key`, or nothing if it's `None` or not in the list, without running
	/// the selection callback: for an app that keeps the selection in its own model.
	pub fn select(&self, key: Option<&K>) {
		let index = key.and_then(|key| self.model.borrow().rows.position(key));
		if index != self.selected_index() {
			self.set_selected(index);
		}
	}

	/// Moves the selection as a navigation key would, whether or not the list has focus, and runs
	/// the selection callback. Returns whether the selection moved.
	#[cfg_attr(
		not(windows),
		expect(clippy::must_use_candidate, reason = "moving the selection is the point; whether it moved is extra")
	)]
	pub fn step(&self, step: Step) -> bool {
		let len = self.len();
		if len == 0 {
			return false;
		}
		let current = self.selected_index();
		let target = step.target(current.unwrap_or(0), len, PAGE);
		if current == Some(target) {
			return false;
		}
		self.set_selected(Some(target));
		run(&self.callbacks.selection_changed);
		true
	}
}

/// Whether Enter or Space is down in the key message being handled. `GetKeyState` reports the
/// keyboard as of that message, so a quick tap that's already released still counts.
#[cfg(windows)]
fn activated_by_keyboard() -> bool {
	use windows_sys::Win32::UI::Input::KeyboardAndMouse::{GetKeyState, VK_RETURN, VK_SPACE};
	unsafe { GetKeyState(i32::from(VK_RETURN)) < 0 || GetKeyState(i32::from(VK_SPACE)) < 0 }
}

#[cfg(not(windows))]
const fn activated_by_keyboard() -> bool {
	false
}
