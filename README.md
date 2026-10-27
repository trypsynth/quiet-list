# quiet-list

A [wxDragon](https://github.com/AllenDang/wxDragon) list of any length whose rows can change without the screen reader reading them out again.

Apps that update a list in the background, such as new mail arriving or a timeline refreshing, usually make the screen reader repeat the selected row on every update, or lose the user's place. In a quiet list, rows are identified by key and the selection follows its row through every update, so the user stays where they were and hears nothing until they move.

| Platform | Control |
|---|---|
| Windows | Drawn by this crate and described to screen readers through [AccessKit](https://github.com/AccessKit/accesskit). Only rows near the selection are in the accessibility tree, each with its position, so "12 of 50,000" still reads right. While the list has focus, the focused row is never sent again; a change to it is sent once focus moves on. |
| macOS, Linux | wxWidgets' native virtual list view. It needs wxWidgets 3.3.4 or later to be accessible on macOS. |

The native list view isn't used on Windows because every change to its row count recreates its accessible children and sends focus to the selected row again, which NVDA reads out each time.

## Usage

Rows can come with their labels:

```rust
use quiet_list::{QuietList, Select};

let label = StaticText::builder(&panel).with_label("&Posts").build();
let posts = QuietList::new(&panel, "Posts");
sizer.add(&label, 0, SizerFlag::All, 8);
sizer.add(&posts, 1, SizerFlag::Expand | SizerFlag::All, 8);

posts.update_labelled(vec![(1, "First post".to_owned()), (2, "Second post".to_owned())], Select::Start);
// Later, in the background: the selection stays on its row, wherever it moved.
posts.update_labelled(rows, Select::Keep);
```

Or the list can ask for labels a page at a time as rows are shown or read, so a list of 50,000 rows shows as quickly as one of 50:

```rust
let messages = QuietList::with_labeler(&panel, "Messages", move |ids: &[MessageId]| store.labels(ids));
messages.update(ids, Select::Start);
```

Create the list's label just before it with the same parent, as for any control, so screen readers on Windows find it, and pass the spoken name to the constructor too.

The list handles the arrow keys, Page Up, Page Down, Home, End, Tab and type-to-find itself. Your app gets the rest through callbacks: `on_selection_changed`, `on_activate` (double-click, or a screen reader's click), `on_context_menu` (right-click, the Applications key or Shift+F10), `on_space`, and `on_key_down`, which sees every key before the list does. `step` moves the selection without the list having focus, for global shortcuts, and `select` moves it without running the selection callback, for apps that keep the selection in their own model.

## wxDragon version

quiet-list needs wxDragon 0.9.24 or later, for `ScrolledWindow::get_view_start`, double-click events, and `navigate` going the right way for Tab.

## License

MIT
