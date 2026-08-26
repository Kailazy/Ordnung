//! The native macOS menu bar.
//!
//! egui draws no OS menu bar, so without this Ordnung shows only the bare
//! "Ordnung / Edit / Window" stub AppKit synthesises for a bundled app — and
//! every keyboard shortcut the app implements is invisible unless you already
//! know it. This module installs a real `NSMenu` whose items *advertise* those
//! shortcuts: macOS renders the key equivalent next to each item, so the menu
//! bar doubles as the app's shortcut documentation.
//!
//! Pure GUI presentation (per `ordnung-architecture`): nothing here touches the
//! catalog or any engine. Menu items never do work themselves — each one parks
//! a [`MenuCommand`] that [`take_command`] hands to `App::update`, which runs
//! exactly the same code path the in-app key handler runs. That keeps one
//! implementation per action instead of a menu copy and a keyboard copy.
//!
//! **Why the key equivalents are real, not decorative.** An `NSMenuItem` with a
//! key equivalent claims that chord *before* it reaches the window, so a menu
//! item and an egui `consume_key` handler for the same chord would both need to
//! exist and only the menu one would ever fire. So the menu owns the chords it
//! displays, and the egui side keeps its handler only where the chord must be
//! context-sensitive (see `App::update`'s Space handling, which stays in egui
//! because it depends on which text field has focus).

use std::sync::atomic::{AtomicU8, Ordering};

/// An action requested from the menu bar, drained once per frame by `App`.
///
/// Deliberately a plain enum with no payload: menu items are global commands,
/// and anything selection-dependent is resolved by `App` when it handles the
/// command, using the selection as of that frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MenuCommand {
    /// Reload the table from the catalog (⌘R).
    Reload,
    /// Open the Settings window (⌘,).
    Settings,
    /// Select every visible row (⌘A).
    SelectAll,
    /// Clear the search box and every column filter (⇧⌘K).
    ClearFilters,
    /// Copy the selected rows as "Artist – Title" lines (⌘C).
    Copy,
    /// Add a folder of music to the catalog (⌘O).
    AddFolder,
    /// Play or pause the selected track (Space is handled in egui; this is the
    /// menu's discoverable equivalent).
    PlayPause,
}

/// Slot holding at most one pending command. A `u8` rather than a channel so the
/// AppKit action can store into it without allocating or locking on the main
/// thread; 0 means "nothing pending". Commands are rare (one per click) and
/// `App` drains this every frame, so a single slot never coalesces two real
/// clicks in practice.
static PENDING: AtomicU8 = AtomicU8::new(0);

fn encode(cmd: MenuCommand) -> u8 {
    match cmd {
        MenuCommand::Reload => 1,
        MenuCommand::Settings => 2,
        MenuCommand::SelectAll => 3,
        MenuCommand::ClearFilters => 4,
        MenuCommand::Copy => 5,
        MenuCommand::AddFolder => 6,
        MenuCommand::PlayPause => 7,
    }
}

fn decode(v: u8) -> Option<MenuCommand> {
    Some(match v {
        1 => MenuCommand::Reload,
        2 => MenuCommand::Settings,
        3 => MenuCommand::SelectAll,
        4 => MenuCommand::ClearFilters,
        5 => MenuCommand::Copy,
        6 => MenuCommand::AddFolder,
        7 => MenuCommand::PlayPause,
        _ => return None,
    })
}

/// Take the pending menu command, if any. Call once per frame from `update`.
pub fn take_command() -> Option<MenuCommand> {
    decode(PENDING.swap(0, Ordering::AcqRel))
}

/// Install the app's menu bar. Call once, early, on the UI thread; a no-op off
/// macOS or when called from a non-main thread.
#[cfg(target_os = "macos")]
pub fn install() {
    imp::install();
}

#[cfg(not(target_os = "macos"))]
pub fn install() {}

#[cfg(target_os = "macos")]
mod imp {
    use super::{encode, MenuCommand, PENDING};
    use std::sync::atomic::Ordering;

    use objc2::declare_class;
    use objc2::msg_send_id;
    use objc2::mutability::MainThreadOnly;
    use objc2::rc::Retained;
    use objc2::runtime::{AnyObject, NSObject, NSObjectProtocol, Sel};
    use objc2::{sel, ClassType, DeclaredClass};
    use objc2_app_kit::{NSApplication, NSEventModifierFlags, NSMenu, NSMenuItem};
    use objc2_foundation::{MainThreadMarker, NSString};

    declare_class!(
        /// The target every Ordnung-specific menu item points at. It holds no
        /// state: the selected item's `tag` *is* the command, so one instance
        /// serves the whole menu bar.
        struct MenuTarget;

        unsafe impl ClassType for MenuTarget {
            type Super = NSObject;
            type Mutability = MainThreadOnly;
            const NAME: &'static str = "OrdnungMenuTarget";
        }

        impl DeclaredClass for MenuTarget {}

        unsafe impl NSObjectProtocol for MenuTarget {}

        unsafe impl MenuTarget {
            /// Park the clicked item's command for `App::update` to pick up.
            /// Doing no work here matters: AppKit calls this from inside menu
            /// tracking, where touching egui state would re-enter the UI while
            /// the run loop is in a modal-ish mode.
            #[method(ordnungMenuAction:)]
            fn menu_action(&self, sender: &AnyObject) {
                let tag: isize = unsafe { objc2::msg_send![sender, tag] };
                if tag > 0 {
                    PENDING.store(tag as u8, Ordering::Release);
                }
            }

            /// Every Ordnung item is always available; AppKit would otherwise
            /// grey items out, since our target implements no
            /// `validateMenuItem:`-style checks.
            #[method(validateMenuItem:)]
            fn validate_menu_item(&self, _item: &AnyObject) -> bool {
                true
            }
        }
    );

    impl MenuTarget {
        fn new(mtm: MainThreadMarker) -> Retained<Self> {
            unsafe { msg_send_id![mtm.alloc::<Self>(), init] }
        }
    }

    /// Build one menu item. `key` is the lowercase key equivalent ("" for none),
    /// `mask` its modifiers, and `cmd` the command it parks — or `None` with an
    /// explicit `selector` for the standard AppKit responders (quit, hide,
    /// close, minimise), which already do the right thing for free.
    fn item(
        mtm: MainThreadMarker,
        title: &str,
        key: &str,
        mask: NSEventModifierFlags,
        cmd: Option<MenuCommand>,
        selector: Option<Sel>,
        target: Option<&MenuTarget>,
    ) -> Retained<NSMenuItem> {
        let sel = selector.unwrap_or_else(|| sel!(ordnungMenuAction:));
        let mi = unsafe {
            NSMenuItem::initWithTitle_action_keyEquivalent(
                mtm.alloc::<NSMenuItem>(),
                &NSString::from_str(title),
                Some(sel),
                &NSString::from_str(key),
            )
        };
        mi.setKeyEquivalentModifierMask(mask);
        if let Some(cmd) = cmd {
            // The tag carries the command, so the shared target needs no state.
            let _: () = unsafe { objc2::msg_send![&*mi, setTag: encode(cmd) as isize] };
            unsafe { mi.setTarget(target.map(|t| t as &AnyObject)) };
        }
        mi
    }

    /// A submenu attached under a top-level bar item.
    fn submenu(mtm: MainThreadMarker, bar: &NSMenu, title: &str) -> Retained<NSMenu> {
        let holder = unsafe {
            NSMenuItem::initWithTitle_action_keyEquivalent(
                mtm.alloc::<NSMenuItem>(),
                &NSString::from_str(title),
                None,
                &NSString::from_str(""),
            )
        };
        let menu =
            unsafe { NSMenu::initWithTitle(mtm.alloc::<NSMenu>(), &NSString::from_str(title)) };
        holder.setSubmenu(Some(&menu));
        bar.addItem(&holder);
        menu
    }

    fn separator(mtm: MainThreadMarker, menu: &NSMenu) {
        menu.addItem(&NSMenuItem::separatorItem(mtm));
    }

    const CMD: NSEventModifierFlags = NSEventModifierFlags::NSEventModifierFlagCommand;
    const NONE: NSEventModifierFlags = NSEventModifierFlags(0);

    pub fn install() {
        let Some(mtm) = MainThreadMarker::new() else {
            return;
        };
        let app = NSApplication::sharedApplication(mtm);
        let target = MenuTarget::new(mtm);
        let bar = unsafe { NSMenu::initWithTitle(mtm.alloc::<NSMenu>(), &NSString::from_str("")) };

        let shift_cmd = NSEventModifierFlags(
            CMD.0 | NSEventModifierFlags::NSEventModifierFlagShift.0,
        );

        // ── Ordnung ────────────────────────────────────────────────────────
        // The first submenu is always the app menu, whatever its title.
        let app_menu = submenu(mtm, &bar, "Ordnung");
        app_menu.addItem(&item(
            mtm,
            "About Ordnung",
            "",
            NONE,
            None,
            Some(sel!(orderFrontStandardAboutPanel:)),
            None,
        ));
        separator(mtm, &app_menu);
        app_menu.addItem(&item(
            mtm,
            "Settings…",
            ",",
            CMD,
            Some(MenuCommand::Settings),
            None,
            Some(&target),
        ));
        separator(mtm, &app_menu);
        app_menu.addItem(&item(
            mtm,
            "Hide Ordnung",
            "h",
            CMD,
            None,
            Some(sel!(hide:)),
            None,
        ));
        app_menu.addItem(&item(
            mtm,
            "Quit Ordnung",
            "q",
            CMD,
            None,
            Some(sel!(terminate:)),
            None,
        ));

        // ── Library ────────────────────────────────────────────────────────
        // The app's own actions, each labelled with the chord that triggers it.
        let library = submenu(mtm, &bar, "Library");
        library.addItem(&item(
            mtm,
            "Add Folder to Library…",
            "o",
            CMD,
            Some(MenuCommand::AddFolder),
            None,
            Some(&target),
        ));
        separator(mtm, &library);
        library.addItem(&item(
            mtm,
            "Reload from Catalog",
            "r",
            CMD,
            Some(MenuCommand::Reload),
            None,
            Some(&target),
        ));

        // ── Edit ───────────────────────────────────────────────────────────
        // Cut/Paste go to the standard responders so text fields keep working;
        // Copy is ours, because in the table it means "copy the selected rows".
        let edit = submenu(mtm, &bar, "Edit");
        edit.addItem(&item(mtm, "Cut", "x", CMD, None, Some(sel!(cut:)), None));
        // Copy likewise: the table's handler already defers to a focused text
        // field, so the chord must reach egui rather than stop at the menu.
        edit.addItem(&item(
            mtm,
            "Copy (⌘C)",
            "",
            NONE,
            Some(MenuCommand::Copy),
            None,
            Some(&target),
        ));
        edit.addItem(&item(
            mtm,
            "Paste",
            "v",
            CMD,
            None,
            Some(sel!(paste:)),
            None,
        ));
        separator(mtm, &edit);
        // ⌘A stays owned by egui: in a focused text field it must keep its
        // "select all text" meaning, and a key equivalent here would claim the
        // chord before the field ever sees it. The item shows the chord (via
        // its own label) and still works as a click.
        edit.addItem(&item(
            mtm,
            "Select All Rows (⌘A)",
            "",
            NONE,
            Some(MenuCommand::SelectAll),
            None,
            Some(&target),
        ));
        edit.addItem(&item(
            mtm,
            "Clear Search & Filters",
            "k",
            shift_cmd,
            Some(MenuCommand::ClearFilters),
            None,
            Some(&target),
        ));

        // ── Playback ───────────────────────────────────────────────────────
        // Space is handled in egui (it must yield to focused text fields), so
        // this item carries no key equivalent — claiming the chord here would
        // steal the space bar from every text field in the app. The label says
        // where the real shortcut lives.
        let playback = submenu(mtm, &bar, "Playback");
        playback.addItem(&item(
            mtm,
            "Play / Pause (Space)",
            "",
            NONE,
            Some(MenuCommand::PlayPause),
            None,
            Some(&target),
        ));

        // ── Window ─────────────────────────────────────────────────────────
        // Handed to AppKit as the Window menu so it auto-populates with the
        // open windows (the video mini-player shows up here).
        let window = submenu(mtm, &bar, "Window");
        window.addItem(&item(
            mtm,
            "Minimize",
            "m",
            CMD,
            None,
            Some(sel!(performMiniaturize:)),
            None,
        ));
        // ⌘W is left to egui, whose handler closes the floating Settings
        // window first when one is open — `performClose:` would go straight
        // for the main window and take the whole app down instead.
        window.addItem(&item(
            mtm,
            "Close Window (⌘W)",
            "",
            NONE,
            None,
            Some(sel!(performClose:)),
            None,
        ));
        unsafe { app.setWindowsMenu(Some(&window)) };

        app.setMainMenu(Some(&bar));

        // Verification hook: `ORDNUNG_DUMP_MENU=1` prints the installed menu
        // bar as the app sees it. Cheap enough to keep — the menu bar is
        // otherwise unobservable from a terminal, since reading it needs
        // Accessibility permission the shell doesn't have.
        if std::env::var_os("ORDNUNG_DUMP_MENU").is_some() {
            dump(&app, mtm);
        }
    }

    /// Print the live main menu (titles + key equivalents) to stdout.
    fn dump(app: &NSApplication, _mtm: MainThreadMarker) {
        let Some(bar) = (unsafe { app.mainMenu() }) else {
            println!("MENU: none installed");
            return;
        };
        for i in 0..unsafe { bar.numberOfItems() } {
            let Some(top) = (unsafe { bar.itemAtIndex(i) }) else {
                continue;
            };
            println!("MENU {}", unsafe { top.title() });
            let Some(sub) = (unsafe { top.submenu() }) else {
                continue;
            };
            for j in 0..unsafe { sub.numberOfItems() } {
                let Some(it) = (unsafe { sub.itemAtIndex(j) }) else {
                    continue;
                };
                if unsafe { it.isSeparatorItem() } {
                    println!("  --");
                    continue;
                }
                let key = unsafe { it.keyEquivalent() }.to_string();
                let mask = unsafe { it.keyEquivalentModifierMask() }.0;
                println!(
                    "  {}  [key={:?} mask={:#x} tag={}]",
                    unsafe { it.title() },
                    key,
                    mask,
                    unsafe { it.tag() }
                );
            }
        }
    }
}
