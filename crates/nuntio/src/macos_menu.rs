//! The macOS menu bar. It replaces winit's default menu, whose Quit item
//! ends the app at once, without asking about programs still running.
//!
//! Items for nuntio's actions carry no key equivalents: their shortcuts
//! stay with the keybindings, which the config can change. Only AppKit's
//! own items (Quit, Hide, Minimize, Full Screen) have them.

use objc2::rc::Retained;
use objc2::runtime::{NSObject, NSObjectProtocol, Sel};
use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::{NSApplication, NSEventModifierFlags, NSMenu, NSMenuItem};
use objc2_foundation::{NSProcessInfo, NSString};
use winit::event_loop::EventLoopProxy;

use crate::actions::Action;
use crate::event::{MenuCommand, UserEvent};
use crate::pane_tree::Direction;

pub struct TargetIvars {
    proxy: EventLoopProxy<UserEvent>,
    /// Indexed by the items' tags.
    commands: Vec<MenuCommand>,
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements, and `Target`
    // doesn't implement `Drop`.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "NuntioMenuTarget"]
    #[ivars = TargetIvars]
    pub struct Target;

    impl Target {
        #[unsafe(method(runCommand:))]
        fn run_command(&self, item: &NSMenuItem) {
            let ivars = self.ivars();
            let command = usize::try_from(item.tag())
                .ok()
                .and_then(|i| ivars.commands.get(i));
            if let Some(&command) = command {
                let _ = ivars.proxy.send_event(UserEvent::Menu(command));
            }
        }
    }

    unsafe impl NSObjectProtocol for Target {}
);

/// Builds the menus; commands are collected for the target's table.
struct Builder {
    mtm: MainThreadMarker,
    commands: Vec<MenuCommand>,
    /// Items that run a command, to point at the target once it exists.
    command_items: Vec<Retained<NSMenuItem>>,
}

impl Builder {
    fn menu(&self, title: &str) -> Retained<NSMenu> {
        NSMenu::initWithTitle(NSMenu::alloc(self.mtm), &NSString::from_str(title))
    }

    fn item(&self, title: &str, action: Option<Sel>, key: &str) -> Retained<NSMenuItem> {
        // SAFETY: the selectors are AppKit's standard actions or our
        // target's `runCommand:`, all taking the sender.
        unsafe {
            NSMenuItem::initWithTitle_action_keyEquivalent(
                NSMenuItem::alloc(self.mtm),
                &NSString::from_str(title),
                action,
                &NSString::from_str(key),
            )
        }
    }

    /// An item that sends `command` to nuntio.
    fn command(&mut self, menu: &NSMenu, title: &str, command: MenuCommand, key: &str) {
        let item = self.item(title, Some(sel!(runCommand:)), key);
        item.setTag(self.commands.len() as isize);
        self.commands.push(command);
        menu.addItem(&item);
        self.command_items.push(item);
    }

    fn action(&mut self, menu: &NSMenu, title: &str, action: Action) {
        self.command(menu, title, MenuCommand::Action(action), "");
    }

    /// An AppKit action, sent along the responder chain.
    fn native(&self, menu: &NSMenu, title: &str, action: Sel, key: &str) -> Retained<NSMenuItem> {
        let item = self.item(title, Some(action), key);
        menu.addItem(&item);
        item
    }

    fn separator(&self, menu: &NSMenu) {
        menu.addItem(&NSMenuItem::separatorItem(self.mtm));
    }

    fn submenu(&self, bar: &NSMenu, menu: &NSMenu) {
        let item = NSMenuItem::new(self.mtm);
        item.setSubmenu(Some(menu));
        bar.addItem(&item);
    }
}

/// The installed menu bar; the items only hold a weak reference to their
/// target, so this keeps it alive.
pub struct MenuBar {
    _target: Retained<Target>,
}

/// Replace the app's menu bar. Must run on the main thread after launch.
pub fn install(proxy: EventLoopProxy<UserEvent>) -> Option<MenuBar> {
    let Some(mtm) = MainThreadMarker::new() else {
        tracing::warn!("the menu bar can only be set up on the main thread");
        return None;
    };
    let app = NSApplication::sharedApplication(mtm);
    let name = NSProcessInfo::processInfo().processName().to_string();
    let mut b = Builder {
        mtm,
        commands: Vec::new(),
        command_items: Vec::new(),
    };
    let bar = b.menu("");

    let app_menu = b.menu(&name);
    b.native(
        &app_menu,
        &format!("About {name}"),
        sel!(orderFrontStandardAboutPanel:),
        "",
    );
    b.separator(&app_menu);
    b.action(&app_menu, "Settings…", Action::OpenSettings);
    b.action(&app_menu, "Reload Config", Action::ReloadConfig);
    b.separator(&app_menu);
    let services = b.menu("Services");
    let services_item = b.item("Services", None, "");
    services_item.setSubmenu(Some(&services));
    app_menu.addItem(&services_item);
    b.separator(&app_menu);
    b.native(&app_menu, &format!("Hide {name}"), sel!(hide:), "h");
    let hide_others = b.native(&app_menu, "Hide Others", sel!(hideOtherApplications:), "h");
    hide_others
        .setKeyEquivalentModifierMask(NSEventModifierFlags::Option | NSEventModifierFlags::Command);
    b.native(&app_menu, "Show All", sel!(unhideAllApplications:), "");
    b.separator(&app_menu);
    b.command(&app_menu, &format!("Quit {name}"), MenuCommand::Quit, "q");
    b.submenu(&bar, &app_menu);

    let shell = b.menu("Shell");
    b.action(&shell, "New Tab", Action::NewTab);
    b.separator(&shell);
    b.action(&shell, "Split Side by Side", Action::SplitVertical);
    b.action(&shell, "Split Top and Bottom", Action::SplitHorizontal);
    b.separator(&shell);
    b.action(&shell, "Close Pane", Action::ClosePane);
    b.action(&shell, "Close Tab", Action::CloseTab);
    b.submenu(&bar, &shell);

    // AppKit adds dictation and the character viewer to a menu titled "Edit".
    let edit = b.menu("Edit");
    b.action(&edit, "Copy", Action::Copy);
    b.action(&edit, "Paste", Action::Paste);
    b.separator(&edit);
    b.action(&edit, "Find…", Action::Search);
    b.action(&edit, "Clear Scrollback", Action::ClearScrollback);
    b.submenu(&bar, &edit);

    let view = b.menu("View");
    b.action(&view, "Bigger", Action::FontIncrease);
    b.action(&view, "Smaller", Action::FontDecrease);
    b.action(&view, "Actual Size", Action::FontReset);
    b.separator(&view);
    b.action(&view, "Zoom Pane", Action::ZoomPane);
    b.separator(&view);
    // AppKit retitles it (Enter/Exit Full Screen) and adds no second one.
    let full_screen = b.native(&view, "Enter Full Screen", sel!(toggleFullScreen:), "f");
    full_screen.setKeyEquivalentModifierMask(
        NSEventModifierFlags::Control | NSEventModifierFlags::Command,
    );
    b.submenu(&bar, &view);

    let window = b.menu("Window");
    b.native(&window, "Minimize", sel!(performMiniaturize:), "m");
    b.native(&window, "Zoom", sel!(performZoom:), "");
    b.separator(&window);
    b.action(&window, "Show Previous Tab", Action::PreviousTab);
    b.action(&window, "Show Next Tab", Action::NextTab);
    b.separator(&window);
    for (title, direction) in [
        ("Select Pane Above", Direction::Up),
        ("Select Pane Below", Direction::Down),
        ("Select Pane on the Left", Direction::Left),
        ("Select Pane on the Right", Direction::Right),
    ] {
        b.action(&window, title, Action::FocusPane(direction));
    }
    b.separator(&window);
    b.native(&window, "Bring All to Front", sel!(arrangeInFront:), "");
    b.submenu(&bar, &window);

    let target = Target::alloc(mtm).set_ivars(TargetIvars {
        proxy,
        commands: std::mem::take(&mut b.commands),
    });
    // SAFETY: NSObject's `init` on a freshly allocated object.
    let target: Retained<Target> = unsafe { msg_send![super(target), init] };
    for item in &b.command_items {
        // SAFETY: `target` implements `runCommand:`, and `MenuBar` keeps
        // it alive as long as the menu may use it.
        unsafe { item.setTarget(Some(&target)) };
    }
    app.setServicesMenu(Some(&services));
    app.setWindowsMenu(Some(&window));
    app.setMainMenu(Some(&bar));
    Some(MenuBar { _target: target })
}
