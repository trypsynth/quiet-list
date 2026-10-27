//! The list on Windows: rows drawn here, described to screen readers through an AccessKit tree
//! that holds only the rows near the selection.
//!
//! The native list view can't be used here. Every change to its row count (`LVM_SETITEMCOUNT`,
//! whatever its flags) makes it recreate its accessible children and send focus to the selected
//! row again, so NVDA read that row out each time rows arrived in the background. Here only what
//! changed is sent, and the focused row is left alone while it keeps focus (see `tree`).

use std::{
	cell::{Cell, RefCell},
	collections::HashMap,
	hash::Hash,
	rc::{Rc, Weak},
	sync::atomic::{AtomicU64, Ordering},
	time::{Duration, Instant},
};

use accesskit::{
	Action, ActionHandler, ActionRequest, ActivationHandler, Node, NodeId, Role, TreeId, TreeInfo, TreeUpdate,
};
use accesskit_windows::{HWND, SubclassingAdapter};
use wxdragon::{
	app::wake_up_idle,
	call_after,
	ffi::{wxd_Window_GetWindowStyle, wxd_Window_SetWindowStyle, wxd_Window_t},
	prelude::*,
	widgets::scrolled_window::ScrollBarConfig,
};

use crate::{
	Select, Step,
	rows::{self, LazyRows},
	tree::{self, ListTree},
};

/// `wxWANTS_CHARS`: arrow keys and Tab come to the list instead of moving between controls.
const WANTS_CHARS: i64 = 0x0004_0000;
/// `wxBORDER_THEME`: the themed border a native list has.
const BORDER_THEME: i64 = 0x1000_0000;
/// Typing within this long of the previous key adds to the search instead of starting over.
const TYPING_PAUSE: Duration = Duration::from_secs(1);
/// Space above and below a row's text, and twice this before it.
const PADDING: i32 = 3;

type Labeler<K> = Box<dyn Fn(&[K]) -> HashMap<K, String>>;
type Callback = RefCell<Option<Rc<dyn Fn()>>>;
type KeyHook = RefCell<Option<Rc<dyn Fn(&WindowEventData) -> bool>>>;
type RequestHandler = Rc<dyn Fn(Request)>;

struct State<K> {
	rows: LazyRows<K>,
	labeler: Option<Labeler<K>>,
	selected: Option<usize>,
	tree: ListTree<K>,
	name: String,
	typed: String,
	typed_at: Option<Instant>,
}

impl<K: Clone + Eq + Hash> State<K> {
	fn label(&mut self, index: usize) -> String {
		let Self { rows, labeler, .. } = self;
		rows.label(index, |keys| labeler.as_ref().map(|labeler| labeler(keys)).unwrap_or_default())
	}

	fn selected_key(&self) -> Option<K> {
		self.rows.key(self.selected?).cloned()
	}

	/// What the screen reader needs to be told. `full` describes the whole tree, for a screen
	/// reader that has just connected; `has_focus` is whether the list has keyboard focus.
	fn tree_update(&mut self, full: bool, has_focus: bool) -> TreeUpdate {
		if full {
			self.tree.reset();
		}
		let len = self.rows.len();
		let mut rows = Vec::new();
		for index in tree::reach(self.selected, len) {
			let Some(key) = self.rows.key(index).cloned() else {
				break;
			};
			let label = self.label(index);
			rows.push((key, index, label));
		}
		let focus = self.selected_key();
		let update = self.tree.update(rows, focus.as_ref(), has_focus);
		let mut nodes: Vec<(NodeId, Node)> =
			update.rows.into_iter().map(|row| (NodeId(row.id), row_node(row))).collect();
		let mut list = Node::new(Role::ListBox);
		list.set_label(self.name.clone());
		// Screen readers take "of N" for each row from the list.
		list.set_size_of_set(len);
		list.set_children(update.children.into_iter().map(NodeId).collect::<Vec<_>>());
		nodes.push((NodeId(tree::ROOT), list));
		TreeUpdate {
			nodes,
			tree: full.then(|| TreeInfo::new(NodeId(tree::ROOT))),
			tree_id: TreeId::ROOT,
			focus: NodeId(update.focus),
		}
	}

	/// Whether letters typed just now are still finding a row.
	fn searching(&self) -> bool {
		!self.typed.is_empty() && self.typed_at.is_some_and(|at| at.elapsed() <= TYPING_PAUSE)
	}

	/// The row type-to-find lands on after `typed`, as in a native list: letters typed in quick
	/// succession find a row starting with all of them, and one letter typed over and over
	/// steps through the rows starting with it.
	fn type_to_find(&mut self, typed: char) -> Option<usize> {
		let len = self.rows.len();
		let now = Instant::now();
		if self.typed_at.is_none_or(|at| now.duration_since(at) > TYPING_PAUSE) {
			self.typed.clear();
		}
		self.typed_at = Some(now);
		if len == 0 || (typed == ' ' && self.typed.is_empty()) {
			return None;
		}
		self.typed.extend(typed.to_lowercase());
		let first = self.typed.chars().next()?;
		let (prefix, from) = if self.typed.chars().all(|c| c == first) {
			(first.to_string(), self.selected.map_or(0, |index| index + 1))
		} else {
			(self.typed.clone(), self.selected.unwrap_or(0))
		};
		(0..len)
			.map(|offset| (from + offset) % len)
			.find(|&index| self.label(index).to_lowercase().starts_with(&prefix))
	}
}

fn row_height(window: ScrolledWindow) -> i32 {
	window.get_char_height() + 2 * PADDING
}

fn row_node(row: tree::Row) -> Node {
	let mut node = Node::new(Role::ListBoxOption);
	node.set_label(row.label);
	node.set_position_in_set(row.index);
	node.add_action(Action::Focus);
	node.add_action(Action::Click);
	node
}

/// Something for a list to do that came from another thread.
enum Request {
	/// A screen reader asked for an action on a row.
	Action(ActionRequest),
	/// The tree couldn't be built when a screen reader connected, so it's sent now.
	FullTree,
}

thread_local! {
	/// The lists on the UI thread, for requests that arrive from AccessKit's threads.
	static LISTS: RefCell<HashMap<u64, RequestHandler>> = RefCell::new(HashMap::new());
}

static NEXT_LIST: AtomicU64 = AtomicU64::new(1);

/// Hands `request` to list `list` on the UI thread.
fn post(list: u64, request: Request) {
	call_after(Box::new(move || {
		let handler = LISTS.with(|lists| lists.borrow().get(&list).cloned());
		if let Some(handler) = handler {
			handler(request);
		}
	}));
	wake_up_idle();
}

/// Runs on a screen reader's request, on one of AccessKit's threads.
struct Actions {
	list: u64,
}

impl ActionHandler for Actions {
	fn do_action(&mut self, request: ActionRequest) {
		post(self.list, Request::Action(request));
	}
}

struct Activation<K> {
	list: u64,
	state: Weak<RefCell<State<K>>>,
	needs_full: Rc<Cell<bool>>,
}

impl<K: Clone + Eq + Hash> ActivationHandler for Activation<K> {
	fn request_initial_tree(&mut self) -> Option<TreeUpdate> {
		let state = self.state.upgrade()?;
		let Ok(mut state) = state.try_borrow_mut() else {
			// Asked while the list was busy. AccessKit shows a placeholder until the next
			// update, which must then be the whole tree.
			self.needs_full.set(true);
			post(self.list, Request::FullTree);
			return None;
		};
		// A full tree describes every row, so whether the list has focus makes no difference.
		Some(state.tree_update(true, true))
	}
}

struct Inner<K> {
	id: u64,
	window: ScrolledWindow,
	state: Rc<RefCell<State<K>>>,
	adapter: RefCell<SubclassingAdapter>,
	needs_full: Rc<Cell<bool>>,
	row_height: Cell<i32>,
	len: Cell<usize>,
	/// The context menu was asked for with the mouse, so it opens where the mouse is.
	menu_by_mouse: Cell<bool>,
	start_at_end: Cell<bool>,
	selection_changed: Callback,
	activate: Callback,
	context_menu: Callback,
	space: Callback,
	key_hook: KeyHook,
}

impl<K> Drop for Inner<K> {
	fn drop(&mut self) {
		let _ = LISTS.try_with(|lists| lists.borrow_mut().remove(&self.id));
	}
}

fn run(callback: &Callback) {
	let callback = callback.borrow().clone();
	if let Some(callback) = callback {
		callback();
	}
}

impl<K: Clone + Eq + Hash + 'static> Inner<K> {
	fn top(&self) -> usize {
		usize::try_from(self.window.get_view_start().1).unwrap_or(0)
	}

	/// Whole rows that fit in the window.
	fn page(&self) -> usize {
		usize::try_from(self.window.get_client_size().height / self.row_height.get()).unwrap_or(0).max(1)
	}

	fn row_at(&self, event: &WindowEventData) -> Option<usize> {
		let WindowEventData::MouseButton(mouse) = event else {
			return None;
		};
		let y = mouse.get_position()?.y;
		let index = self.top() + usize::try_from(y / self.row_height.get()).ok()?;
		(index < self.len.get()).then_some(index)
	}

	/// Tells the screen reader what changed.
	fn push_tree(&self) {
		self.push_tree_with(self.window.has_focus());
	}

	fn push_tree_with(&self, has_focus: bool) {
		let full = self.needs_full.replace(false);
		let events = {
			let Ok(mut adapter) = self.adapter.try_borrow_mut() else {
				return;
			};
			adapter.update_if_active(|| self.state.borrow_mut().tree_update(full, has_focus))
		};
		if let Some(events) = events {
			events.raise();
		}
	}

	fn scroll_to(&self, index: usize) {
		let top = self.top();
		let page = self.page();
		let top = if index < top {
			index
		} else if index >= top + page {
			index + 1 - page
		} else {
			return;
		};
		self.window.scroll_coords(-1, i32::try_from(top).unwrap_or(i32::MAX));
	}

	fn set_len(&self, len: usize) {
		if self.len.replace(len) != len {
			self.set_scrollbars();
		}
	}

	/// One scroll unit per row, so the view start is the top row.
	fn set_scrollbars(&self) {
		let len = self.len.get();
		let top = self.top().min(len.saturating_sub(1));
		self.window.set_scrollbars(ScrollBarConfig {
			pixels_per_unit_x: 0,
			pixels_per_unit_y: self.row_height.get(),
			no_units_x: 0,
			no_units_y: i32::try_from(len).unwrap_or(i32::MAX),
			x_pos: 0,
			y_pos: i32::try_from(top).unwrap_or(0),
			no_refresh: true,
		});
	}

	/// Moves the selection to `index`, or clears it. `notify` runs the selection callback, for a
	/// move the user made.
	fn set_selected(&self, index: Option<usize>, notify: bool) {
		{
			let mut state = self.state.borrow_mut();
			if index.is_some_and(|index| index >= state.rows.len()) || state.selected == index {
				return;
			}
			state.selected = index;
		}
		if let Some(index) = index {
			self.scroll_to(index);
		}
		self.window.refresh(false, None);
		self.push_tree();
		if notify {
			run(&self.selection_changed);
		}
	}

	fn step(&self, step: Step) -> bool {
		let len = self.len.get();
		if len == 0 {
			return false;
		}
		let current = self.state.borrow().selected;
		let target = step.target(current.unwrap_or(0), len, self.page());
		self.set_selected(Some(target), true);
		current != Some(target)
	}

	fn handle(&self, request: Request) {
		match request {
			Request::FullTree => self.push_tree(),
			Request::Action(request) => {
				let index = {
					let state = self.state.borrow();
					state.tree.key_of(request.target_node.0).and_then(|key| state.rows.position(key))
				};
				let Some(index) = index else { return };
				match request.action {
					Action::Focus => {
						self.window.set_focus();
						self.set_selected(Some(index), true);
					}
					Action::Click => {
						self.window.set_focus();
						self.set_selected(Some(index), true);
						run(&self.activate);
					}
					_ => {}
				}
			}
		}
	}

	// wxDragon passes every event on unless the handler says otherwise, so each handler here
	// stops the ones it handled: `wxScrolledWindow` would otherwise scroll on the arrow keys too,
	// and Tab would move focus twice.

	fn paint(&self, event: &WindowEventData) {
		event.skip(false);
		let dc = AutoBufferedPaintDC::new(&self.window);
		dc.set_background(SystemSettings::get_colour(SystemColour::Window));
		dc.clear();
		let Ok(mut state) = self.state.try_borrow_mut() else {
			return;
		};
		if let Some(font) = self.window.get_font() {
			dc.set_font(&font);
		}
		let width = self.window.get_client_size().width;
		let text_top = (self.row_height.get() - dc.get_char_height()) / 2;
		let focused = self.window.has_focus();
		let top = self.top();
		let end = (top + self.page() + 1).min(state.rows.len());
		for index in top..end {
			let y = i32::try_from(index - top).unwrap_or(0) * self.row_height.get();
			let label = state.label(index);
			let text = if state.selected == Some(index) {
				let (back, text) = if focused {
					(SystemColour::Highlight, SystemColour::HighlightText)
				} else {
					(SystemColour::BtnFace, SystemColour::BtnText)
				};
				let back = SystemSettings::get_colour(back);
				dc.set_brush(back, BrushStyle::Solid);
				dc.set_pen(back, 1, PenStyle::Solid);
				dc.draw_rectangle(0, y, width, self.row_height.get());
				text
			} else {
				SystemColour::WindowText
			};
			dc.set_text_foreground(SystemSettings::get_colour(text));
			dc.draw_text(&label, 2 * PADDING, y + text_top);
		}
	}

	fn key_down(&self, event: &WindowEventData) {
		let WindowEventData::Keyboard(key) = event else {
			return;
		};
		let hook = self.key_hook.borrow().clone();
		if hook.is_some_and(|hook| hook(event)) {
			event.skip(false);
			return;
		}
		let code = key.get_key_code().unwrap_or(0);
		let (control, alt, shift) = (key.control_down(), key.alt_down(), key.shift_down());
		if code == WXK_TAB && !control && !alt {
			event.skip(false);
			self.window.navigate(!shift);
			return;
		}
		if code == WXK_WINDOWS_MENU || (code == WXK_F10 && shift && !control && !alt) {
			event.skip(false);
			run(&self.context_menu);
			return;
		}
		match Step::from_key(code) {
			Some(step) if !alt => {
				event.skip(false);
				self.step(step);
			}
			_ => event.skip(true),
		}
	}

	fn char(&self, event: &WindowEventData) {
		let WindowEventData::Keyboard(key) = event else {
			return;
		};
		let typed = key
			.get_unicode_key()
			.and_then(|code| u32::try_from(code).ok())
			.and_then(char::from_u32)
			.filter(|typed| !typed.is_control());
		let Some(typed) = typed.filter(|_| !key.control_down() && !key.alt_down()) else {
			event.skip(true);
			return;
		};
		event.skip(false);
		if typed == ' ' && !self.state.borrow().searching() {
			run(&self.space);
			return;
		}
		let found = self.state.borrow_mut().type_to_find(typed);
		if let Some(index) = found {
			self.set_selected(Some(index), true);
		}
	}

	fn pressed(&self, event: &WindowEventData) {
		self.window.set_focus();
		if let Some(index) = self.row_at(event) {
			self.set_selected(Some(index), true);
		}
		event.skip(true);
	}

	fn double_clicked(&self, event: &WindowEventData) {
		let row = self.row_at(event);
		let on_selection = row.is_some() && self.state.borrow().selected == row;
		event.skip(!on_selection);
		if on_selection {
			run(&self.activate);
		}
	}

	fn released_right(&self, event: &WindowEventData) {
		let on_row = self.row_at(event).is_some();
		event.skip(!on_row);
		if on_row {
			self.menu_by_mouse.set(true);
			run(&self.context_menu);
			self.menu_by_mouse.set(false);
		}
	}

	/// Leaving the list brings the focused row up to date (a change made to it while it had
	/// focus was held back), so a screen reader reads it as it is now when the user comes back.
	fn focus_left(&self, event: &WindowEventData) {
		self.push_tree_with(false);
		self.repaint(event);
	}

	fn repaint(&self, event: &WindowEventData) {
		self.window.refresh(false, None);
		event.skip(true);
	}

	/// Moving to a monitor with a different scale resizes the window and changes its font, so
	/// the rows change height too.
	fn resized(&self, event: &WindowEventData) {
		let height = row_height(self.window);
		if self.row_height.replace(height) != height {
			self.set_scrollbars();
		}
		self.repaint(event);
	}

	/// Replaces the rows with `replace`, then selects as `select` says.
	fn update(&self, replace: impl FnOnce(&mut LazyRows<K>), select: Select<K>) {
		let fresh = select == Select::Start;
		let (len, target, moved, key_changed) = {
			let mut state = self.state.borrow_mut();
			let previous = state.selected.and_then(|index| state.rows.key(index).cloned().map(|key| (key, index)));
			replace(&mut state.rows);
			let last = state.rows.len().checked_sub(1);
			let start = if self.start_at_end.get() { last } else { last.map(|_| 0) };
			let target = match select {
				Select::Start => start,
				Select::Key(key) => state.rows.position(&key).or(start),
				Select::Keep if previous.is_none() => start,
				Select::Keep => rows::reselect(previous.as_ref().map(|(key, index)| (key, *index)), state.rows.keys()),
			};
			state.selected = target;
			let moved = previous.as_ref().map(|(_, index)| *index) != target;
			let key_changed = fresh || previous.map(|(key, _)| key) != state.selected_key();
			(state.rows.len(), target, moved, key_changed)
		};
		self.set_len(len);
		if fresh {
			self.window.scroll_coords(-1, 0);
		}
		if let Some(target) = target
			&& (fresh || moved)
		{
			self.scroll_to(target);
		}
		self.window.refresh(false, None);
		self.push_tree();
		if key_changed {
			run(&self.selection_changed);
		}
	}
}

/// A list of rows identified by keys of type `K`.
pub struct QuietList<K> {
	inner: Rc<Inner<K>>,
}

impl<K: 'static> WxWidget for QuietList<K> {
	fn handle_ptr(&self) -> *mut wxd_Window_t {
		self.inner.window.handle_ptr()
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
		let window = ScrolledWindow::builder(parent).with_style(ScrolledWindowStyle::VScroll).build();
		// SAFETY: the window was just created, so its handle is valid.
		unsafe {
			let handle = window.handle_ptr();
			let style = wxd_Window_GetWindowStyle(handle);
			wxd_Window_SetWindowStyle(handle, style | WANTS_CHARS | BORDER_THEME);
		}
		window.set_background_style(BackgroundStyle::Paint);
		let state = Rc::new(RefCell::new(State {
			rows: LazyRows::default(),
			labeler,
			selected: None,
			tree: ListTree::default(),
			name: name.to_owned(),
			typed: String::new(),
			typed_at: None,
		}));
		let id = NEXT_LIST.fetch_add(1, Ordering::Relaxed);
		let needs_full = Rc::new(Cell::new(false));
		// The adapter answers screen readers for this window. It has to exist before the window
		// is first shown.
		let adapter = SubclassingAdapter::new(
			HWND(window.get_handle()),
			Activation { list: id, state: Rc::downgrade(&state), needs_full: needs_full.clone() },
			Actions { list: id },
		);
		let inner = Rc::new(Inner {
			id,
			window,
			state,
			adapter: RefCell::new(adapter),
			needs_full,
			row_height: Cell::new(row_height(window)),
			len: Cell::new(0),
			menu_by_mouse: Cell::new(false),
			start_at_end: Cell::new(false),
			selection_changed: RefCell::new(None),
			activate: RefCell::new(None),
			context_menu: RefCell::new(None),
			space: RefCell::new(None),
			key_hook: RefCell::new(None),
		});
		let weak = Rc::downgrade(&inner);
		let handler: RequestHandler = Rc::new(move |request| {
			if let Some(inner) = weak.upgrade() {
				inner.handle(request);
			}
		});
		LISTS.with(|lists| lists.borrow_mut().insert(id, handler));
		Self::bind(&inner);
		Self { inner }
	}

	fn bind(inner: &Rc<Inner<K>>) {
		let window = inner.window;
		let with = |action: fn(&Inner<K>, &WindowEventData)| {
			let weak = Rc::downgrade(inner);
			move |event: WindowEventData| {
				if let Some(inner) = weak.upgrade() {
					action(&inner, &event);
				}
			}
		};
		window.on_paint(with(Inner::paint));
		window.on_key_down(with(Inner::key_down));
		window.on_char(with(Inner::char));
		window.on_mouse_left_down(with(Inner::pressed));
		window.on_mouse_right_down(with(Inner::pressed));
		window.on_mouse_right_up(with(Inner::released_right));
		window.on_mouse_left_double_click(with(Inner::double_clicked));
		// The selection is drawn in a different colour while the list has focus.
		window.on_set_focus(with(Inner::repaint));
		window.on_kill_focus(with(Inner::focus_left));
		window.on_size(with(Inner::resized));
	}

	/// Whether the list was created and can show rows.
	#[must_use]
	pub fn ready(&self) -> bool {
		self.inner.window.is_valid()
	}

	/// Shows `menu` under the selected row, or where the mouse is if it was asked for with the
	/// mouse.
	pub fn popup_menu(&self, menu: &mut Menu) {
		let inner = &self.inner;
		let selected = inner.state.borrow().selected;
		let top = inner.top();
		let position = selected
			.filter(|index| !inner.menu_by_mouse.get() && (top..top + inner.page()).contains(index))
			.map(|index| {
				let rows = i32::try_from(index - top + 1).unwrap_or(0);
				inner.window.client_to_screen(Point::new(2 * PADDING, rows * inner.row_height.get()))
			});
		inner.window.popup_menu(menu, position);
	}

	/// Runs `changed` whenever the selected row changes, or the selection is cleared, except
	/// through [`select`](Self::select).
	pub fn on_selection_changed(&self, changed: impl Fn() + 'static) {
		*self.inner.selection_changed.borrow_mut() = Some(Rc::new(changed));
	}

	/// Runs `activate` when a row is double-clicked, or a screen reader clicks it. Enter is left
	/// to the app, which usually has it on a menu item already.
	pub fn on_activate(&self, activate: impl Fn() + 'static) {
		*self.inner.activate.borrow_mut() = Some(Rc::new(activate));
	}

	/// Runs `show` when a row is right-clicked, or the Applications key or Shift+F10 is pressed
	/// in the list. Show the menu with [`popup_menu`](Self::popup_menu).
	pub fn on_context_menu(&self, show: impl Fn() + 'static) {
		*self.inner.context_menu.borrow_mut() = Some(Rc::new(show));
	}

	/// Runs `pressed` when Space is pressed on its own in the list. Space typed while finding a
	/// row by its first letters stays part of what's being found.
	pub fn on_space(&self, pressed: impl Fn() + 'static) {
		*self.inner.space.borrow_mut() = Some(Rc::new(pressed));
	}

	/// Shows `hook` every key pressed in the list before the list acts on it. It returns whether
	/// it handled the key, in which case the list leaves the key alone, and no character is
	/// typed for it.
	pub fn on_key_down(&self, hook: impl Fn(&WindowEventData) -> bool + 'static) {
		*self.inner.key_hook.borrow_mut() = Some(Rc::new(hook));
	}

	/// The selected row's key.
	#[must_use]
	pub fn selected(&self) -> Option<K> {
		self.inner.state.borrow().selected_key()
	}

	/// The selected row's position in the list.
	#[must_use]
	pub fn selected_index(&self) -> Option<usize> {
		self.inner.state.borrow().selected
	}

	/// The selected row's label, as the list shows it.
	#[must_use]
	pub fn selected_label(&self) -> Option<String> {
		let mut state = self.inner.state.borrow_mut();
		let index = state.selected?;
		Some(state.label(index))
	}

	#[must_use]
	pub fn len(&self) -> usize {
		self.inner.state.borrow().rows.len()
	}

	#[must_use]
	pub fn is_empty(&self) -> bool {
		self.inner.state.borrow().rows.is_empty()
	}

	/// Whether a list that shows something new starts on its last row instead of its first.
	pub fn set_start_at_end(&self, at_end: bool) {
		self.inner.start_at_end.set(at_end);
	}

	/// Replaces the rows, to be labelled by the labeler as they're shown. A key that's already in
	/// the list is left out. The selection callback runs if the selected row changed.
	pub fn update(&self, keys: Vec<K>, select: Select<K>) {
		self.inner.update(|rows| rows.replace(keys), select);
	}

	/// Replaces the rows with ones that come with their labels. A key that's already in the list
	/// is left out. The selection callback runs if the selected row changed.
	pub fn update_labelled(&self, rows: Vec<(K, String)>, select: Select<K>) {
		self.inner.update(|current| current.replace_labelled(rows), select);
	}

	/// Selects the row with `key`, or nothing if it's `None` or not in the list, without running
	/// the selection callback: for an app that keeps the selection in its own model.
	pub fn select(&self, key: Option<&K>) {
		let index = key.and_then(|key| self.inner.state.borrow().rows.position(key));
		self.inner.set_selected(index, false);
	}

	/// Moves the selection as a navigation key would, whether or not the list has focus, and runs
	/// the selection callback. Returns whether the selection moved.
	#[expect(clippy::must_use_candidate, reason = "moving the selection is the point; whether it moved is extra")]
	pub fn step(&self, step: Step) -> bool {
		self.inner.step(step)
	}
}
