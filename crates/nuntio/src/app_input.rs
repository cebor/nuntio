//! Keyboard and mouse input, shortcuts and their actions, pasting and
//! dropped files.

use std::path::Path;
use std::time::Instant;

use nuntio_config::detect;
use nuntio_config::{OptionAsMeta, StatusItem};
use nuntio_term::{GridPoint, SelectionKind, TermMode};
use winit::dpi::PhysicalPosition;
use winit::event::{Modifiers, MouseButton};
use winit::keyboard::{Key, ModifiersKeyState, ModifiersState, NamedKey};
use winit::window::{CursorIcon, ResizeDirection, Window};

use super::app_panes::CloseTarget;
use super::{CONFIRM_REPEAT, Core};
use crate::actions::Action;
use crate::actions_menu::{MenuKind, Step};
use crate::banner::{Banner, Severity};
use crate::input::{self, KeyEventKind, KeyInput, KeyPress};
use crate::mouse::{Button, MULTI_CLICK_INTERVAL, MouseAction};
use crate::pane_tree::Axis;
use crate::shell_words::{PathSyntax, dropped_path};
use crate::shells::{self, Launch};
use crate::tab_bar::BarHit;
use crate::update;
use crate::window::{TabDrag, WindowState};

/// Title of the banner that asks to confirm a paste.
const PASTE_BANNER: &str = "Paste";

/// A paste that would run commands at once and waits to be repeated.
#[derive(Debug, Clone)]
pub struct PendingPaste {
    text: String,
    until: Instant,
}

/// Why pasting `text` without bracketed paste needs confirmation: each
/// line break works like Enter, so the shell runs the lines at once.
fn paste_warning(text: &str) -> Option<String> {
    if !text.contains(['\n', '\r']) {
        return None;
    }
    let lines = text.lines().filter(|line| !line.trim().is_empty()).count();
    Some(if lines > 1 {
        format!("{lines} lines would run at once; paste again to run them")
    } else {
        "the text has a line break and would run at once; paste again to run it".into()
    })
}

/// Whether Alt should act as Meta (ESC prefix). On macOS Option composes
/// characters unless Option-as-Meta is enabled for the pressed side.
fn alt_is_meta(mods: &Modifiers, option_as_meta: OptionAsMeta) -> bool {
    if !cfg!(target_os = "macos") {
        return mods.state().alt_key();
    }
    let left = mods.lalt_state() == ModifiersKeyState::Pressed;
    let right = mods.ralt_state() == ModifiersKeyState::Pressed;
    match option_as_meta {
        OptionAsMeta::None => false,
        OptionAsMeta::Left => left,
        OptionAsMeta::Right => right,
        OptionAsMeta::Both => left || right,
    }
}

fn resize_cursor(direction: ResizeDirection) -> CursorIcon {
    match direction {
        ResizeDirection::East => CursorIcon::EResize,
        ResizeDirection::West => CursorIcon::WResize,
        ResizeDirection::North => CursorIcon::NResize,
        ResizeDirection::South => CursorIcon::SResize,
        ResizeDirection::NorthEast => CursorIcon::NeResize,
        ResizeDirection::NorthWest => CursorIcon::NwResize,
        ResizeDirection::SouthEast => CursorIcon::SeResize,
        ResizeDirection::SouthWest => CursorIcon::SwResize,
    }
}

/// Where the mouse pointer is in the window, asked from the system. Needed
/// for drops: while something is dragged, the window gets no pointer moves.
#[cfg(target_os = "macos")]
fn cursor_position(window: &Window) -> Option<PhysicalPosition<f64>> {
    let view = super::ns_view(window)?;
    let in_window = view.window()?.mouseLocationOutsideOfEventStream();
    let point = view.convertPoint_fromView(in_window, None);
    // AppKit counts y from the bottom unless the view is flipped.
    let y = if view.isFlipped() {
        point.y
    } else {
        view.bounds().size.height - point.y
    };
    Some(winit::dpi::LogicalPosition::new(point.x, y).to_physical(window.scale_factor()))
}

#[cfg(windows)]
fn cursor_position(window: &Window) -> Option<PhysicalPosition<f64>> {
    use windows_sys::Win32::Foundation::POINT;
    use windows_sys::Win32::Graphics::Gdi::ScreenToClient;
    use windows_sys::Win32::UI::WindowsAndMessaging::GetCursorPos;
    use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};

    let RawWindowHandle::Win32(handle) = window.window_handle().ok()?.as_raw() else {
        return None;
    };
    let mut point = POINT { x: 0, y: 0 };
    // SAFETY: `hwnd` is our live window and `point` outlives the calls.
    let found = unsafe {
        GetCursorPos(&mut point) != 0 && ScreenToClient(handle.hwnd.get() as _, &mut point) != 0
    };
    found.then(|| PhysicalPosition::new(f64::from(point.x), f64::from(point.y)))
}

/// Elsewhere the last pointer position is used.
#[cfg(not(any(target_os = "macos", windows)))]
fn cursor_position(_window: &Window) -> Option<PhysicalPosition<f64>> {
    None
}

impl Core {
    // ----- Actions -----------------------------------------------------------

    pub(super) fn run_action(&mut self, state: &mut WindowState, action: Action) {
        match action {
            Action::Copy => self.copy_selection(state),
            Action::Paste => self.paste_clipboard(state),
            Action::ScrollPageUp => state.term().scroll_page(true),
            Action::ScrollPageDown => state.term().scroll_page(false),
            Action::ScrollLineUp => state.term().scroll(1),
            Action::ScrollLineDown => state.term().scroll(-1),
            Action::ClearScrollback => state.term().clear_history(),
            Action::FontIncrease => self.set_font_size(state, self.font_size + 1.0),
            Action::FontDecrease => self.set_font_size(state, self.font_size - 1.0),
            Action::FontReset => self.set_font_size(state, self.config.font.size),
            Action::NewTab => self.new_tab(state, self.default_launch()),
            Action::OpenSettings => self.open_settings(state),
            Action::CheckForUpdates => update::check_now(self.proxy.clone()),
            Action::CloseTab => {
                let id = state.content().focused;
                self.request_close(state, CloseTarget::Tab(id));
            }
            Action::ClosePane => {
                let id = state.content().focused;
                self.request_close(state, CloseTarget::Pane(id));
            }
            Action::NextTab => state.select_next_tab(),
            Action::PreviousTab => state.select_previous_tab(),
            Action::SelectTab(index) => state.select_tab(index),
            Action::ReloadConfig => self.reload_config(Some(state)),
            Action::SplitVertical => self.split(state, Axis::Vertical),
            Action::SplitHorizontal => self.split(state, Axis::Horizontal),
            Action::FocusPane(direction) => state.focus_neighbor(&self.config, direction),
            Action::ResizePane(direction) => state.resize_pane(&self.config, direction),
            Action::ZoomPane => state.toggle_zoom(&self.config),
            Action::Search => state.open_search(),
            Action::ToggleFullscreen => state.toggle_fullscreen(),
            Action::OpenActionsMenu if state.menu_kind() == Some(MenuKind::Actions) => {
                state.close_actions_menu();
            }
            Action::OpenActionsMenu => {
                state.open_actions_menu(&self.config, &self.stats, &self.bindings);
            }
            Action::OpenShellMenu if state.menu_kind() == Some(MenuKind::Shells) => {
                state.close_actions_menu();
            }
            Action::OpenShellMenu => self.open_shell_menu(state, false),
            Action::NewTabWithShell(index) => {
                if let Some(choice) = self.shell_choices.get(index).cloned() {
                    self.new_tab(state, Launch::Shell(choice));
                }
            }
        }
        state.window.request_redraw();
    }

    /// Open the menu of shells for a new tab, below the tab bar's "+" with
    /// `at_new_tab`, otherwise at the `shell` status bar item. Looks for
    /// installed shells each time, so new ones show up.
    pub(super) fn open_shell_menu(&mut self, state: &mut WindowState, at_new_tab: bool) {
        self.shell_choices = shells::choices(
            &self.config,
            detect::installed_shells(),
            detect::wsl_distributions(),
        );
        let names: Vec<String> = self.shell_choices.iter().map(|c| c.name.clone()).collect();
        state.open_shell_menu(
            &self.config,
            &self.stats,
            &self.bindings,
            &names,
            at_new_tab,
        );
    }

    /// Open `nuntio-config` in a new tab.
    fn open_settings(&mut self, state: &mut WindowState) {
        match crate::pane_env::helper() {
            Some(helper) => {
                self.new_tab(
                    state,
                    Launch::Settings(helper.to_string_lossy().into_owned()),
                );
            }
            None => self.notify(Banner::new(
                Severity::Warning,
                "Settings",
                vec!["nuntio-config is not installed next to nuntio".into()],
            )),
        }
    }

    // ----- Clipboard and pasting -------------------------------------------

    fn copy_selection(&mut self, state: &WindowState) {
        if let Some(text) = state.term().selection_text() {
            self.set_clipboard(text);
        }
    }

    fn paste_clipboard(&mut self, state: &mut WindowState) {
        let Some(text) = self.clipboard_text() else {
            return;
        };
        // Pasting into the find bar extends the query.
        if !state.search_append(text.lines().next().unwrap_or("")) {
            self.paste(state, text);
        }
    }

    /// Paste into the focused pane. Where the program doesn't use
    /// bracketed paste, text with line breaks would run at once: the
    /// first paste only warns, and the same paste again within
    /// `CONFIRM_REPEAT` goes through.
    pub(super) fn paste(&mut self, state: &WindowState, text: String) {
        let bracketed = state.term().mode().contains(TermMode::BRACKETED_PASTE);
        let pending = self.pending_paste.take();
        self.dismiss_banner(PASTE_BANNER);
        let now = Instant::now();
        if self.config.confirm_paste
            && !bracketed
            && let Some(message) = paste_warning(&text)
            && !pending.is_some_and(|p| p.text == text && now < p.until)
        {
            self.pending_paste = Some(PendingPaste {
                text,
                until: now + CONFIRM_REPEAT,
            });
            self.ask_to_repeat(PASTE_BANNER, message);
            return;
        }
        state.term().paste(&text);
    }

    /// Paste a dropped file's path into the pane under the pointer.
    pub(super) fn drop_file(&mut self, state: &mut WindowState, path: &Path) {
        if let Some(pos) = cursor_position(&state.window).or(state.mouse.position)
            && let Some(id) = state.pane_at(&self.config, pos)
        {
            state.focus_pane(id);
        }
        let pane = state.content().focused_pane();
        let syntax = PathSyntax::of(pane.wsl.is_some(), &pane.term.process_name());
        state
            .term()
            .paste(&dropped_path(&path.to_string_lossy(), syntax));
        state.window.request_redraw();
    }

    // ----- Keyboard ----------------------------------------------------------

    pub(crate) fn keyboard_input(&mut self, state: &mut WindowState, event: &KeyPress) {
        let kind = event.kind;
        // The release of a key nuntio used is nuntio's too.
        let used = state.used_keys.remove(&event.physical);
        if kind == KeyEventKind::Release && used {
            return;
        }
        let mods = state.modifiers.state();
        if kind != KeyEventKind::Release && !self.key_for_program(state, event, mods) {
            state.used_keys.insert(event.physical);
            return;
        }
        let meta = alt_is_meta(&state.modifiers, self.config.macos.option_as_meta);
        let key_input = KeyInput::new(event, mods, meta);
        let Some(bytes) = input::encode_key(&key_input, state.term().mode()) else {
            return;
        };
        match kind {
            // Letting go of a key doesn't count as typing.
            KeyEventKind::Release => state.term().send(bytes),
            _ => state.type_bytes(bytes),
        }
    }

    /// Runs the find bar and shortcuts for a pressed key. Returns whether
    /// the key is left for the program in the focused pane.
    fn key_for_program(
        &mut self,
        state: &mut WindowState,
        event: &KeyPress,
        mods: ModifiersState,
    ) -> bool {
        if state.actions_menu.is_some() {
            self.actions_menu_key(state, event, mods);
            return false;
        }
        if state.search.is_some() && self.search_key(state, event) {
            return false;
        }
        let latin = input::latin_key(&event.unmodified, event.physical);
        if let Some(action) = self.lookup_binding(&event.unmodified, latin.as_ref(), mods) {
            self.run_action(state, action);
            return false;
        }
        // Unbound Cmd/Super combinations are shortcuts, not text. The
        // modifier keys themselves still go to programs that ask for them
        // (kitty's "report all keys").
        !mods.super_key() || input::is_modifier(&event.logical)
    }

    /// The shortcut for a key; on layouts without Latin letters, also
    /// looked up by the key's US character (see `input::latin_key`).
    fn lookup_binding(
        &self,
        unmodified: &Key,
        latin: Option<&Key>,
        mods: ModifiersState,
    ) -> Option<Action> {
        self.bindings
            .lookup(unmodified, mods)
            .or_else(|| self.bindings.lookup(latin?, mods))
    }

    /// Keys for the open actions menu, which takes all of them: the arrows
    /// move, Enter runs the selected entry and Escape closes. Only the
    /// shortcut that opens the menu closes it again.
    fn actions_menu_key(
        &mut self,
        state: &mut WindowState,
        event: &KeyPress,
        mods: ModifiersState,
    ) {
        let view = state.menu_view();
        let Some(menu) = state.actions_menu.as_mut() else {
            return;
        };
        let opener = menu.kind().opener();
        let mut run = None;
        match &event.logical {
            Key::Named(NamedKey::Escape) => state.close_actions_menu(),
            Key::Named(NamedKey::ArrowDown) => menu.select(Step::Next, view),
            Key::Named(NamedKey::ArrowUp) => menu.select(Step::Previous, view),
            Key::Named(NamedKey::Home) => menu.select(Step::First, view),
            Key::Named(NamedKey::End) => menu.select(Step::Last, view),
            Key::Named(NamedKey::Enter) => {
                run = menu.selected_action();
                if run.is_some() {
                    state.close_actions_menu();
                }
            }
            _ => {
                let latin = input::latin_key(&event.unmodified, event.physical);
                let opens =
                    self.lookup_binding(&event.unmodified, latin.as_ref(), mods) == Some(opener);
                if opens {
                    state.close_actions_menu();
                }
            }
        }
        if let Some(action) = run {
            self.run_action(state, action);
        }
        state.window.request_redraw();
    }

    /// Keys for the open find bar. Returns whether the key was used;
    /// shortcuts not handled here still work while searching.
    fn search_key(&mut self, state: &mut WindowState, event: &KeyPress) -> bool {
        let mods = state.modifiers.state();
        let term = &state.tabs.active().content.focused_pane().term;
        let Some(bar) = state.search.as_mut() else {
            return false;
        };
        // On macOS, Option+R arrives as "®"; the toggle must see the "r",
        // also where the key gives a non-Latin letter.
        let unmodified = &event.unmodified;
        let latin = input::latin_key(unmodified, event.physical);
        let is_r = matches!(latin.as_ref().unwrap_or(unmodified), Key::Character(c) if c.eq_ignore_ascii_case("r"));
        match &event.logical {
            Key::Named(NamedKey::Escape) => state.search = None,
            Key::Named(NamedKey::Enter) => bar.next(term, !mods.shift_key()),
            Key::Named(NamedKey::ArrowUp) => bar.next(term, true),
            Key::Named(NamedKey::ArrowDown) => bar.next(term, false),
            Key::Named(NamedKey::Backspace) => bar.backspace(term),
            // Alt+R toggles regex mode, like in many editors.
            _ if mods.alt_key() && is_r => bar.toggle_regex(term),
            _ => {
                let meta = alt_is_meta(&state.modifiers, self.config.macos.option_as_meta);
                let Some(text) = input::field_text(&KeyInput::new(event, mods, meta)) else {
                    // Shortcuts still work; other keys (Tab, F1, Ctrl+W, …)
                    // must not reach the shell behind the bar.
                    return self
                        .lookup_binding(unmodified, latin.as_ref(), mods)
                        .is_none();
                };
                bar.append(text, term);
            }
        }
        state.window.request_redraw();
        true
    }

    // ----- Mouse -------------------------------------------------------------

    pub(super) fn mouse_input(
        &mut self,
        state: &mut WindowState,
        button: MouseButton,
        pressed: bool,
    ) {
        // Open the link that is under the pointer now, not the one that
        // was there when it last moved.
        if pressed {
            state.update_hover_link(&self.config);
        }
        let button = match button {
            MouseButton::Left => Button::Left,
            MouseButton::Middle => Button::Middle,
            MouseButton::Right => Button::Right,
            _ => return,
        };
        match (state.mouse.position, pressed) {
            (Some(pos), true) => self.mouse_press(state, button, pos),
            (Some(pos), false) => self.mouse_release(state, button, pos),
            // The pointer left the window; a release still ends what the
            // press started.
            (None, false) => self.release_outside(state, button),
            (None, true) => {}
        }
    }

    fn mouse_press(&mut self, state: &mut WindowState, button: Button, pos: PhysicalPosition<f64>) {
        if state.actions_menu.is_some() {
            self.press_actions_menu(state, button, pos);
            return;
        }
        if self.press_banner(state, pos) || self.press_window_chrome(state, button, pos) {
            return;
        }
        let left = button == Button::Left;
        match state.status_item_at(&self.config, &self.stats, pos) {
            Some(StatusItem::Update) if left => {
                self.open_update();
                return;
            }
            Some(StatusItem::Actions) if left => {
                self.run_action(state, Action::OpenActionsMenu);
                return;
            }
            Some(StatusItem::Shell) if left => {
                self.run_action(state, Action::OpenShellMenu);
                return;
            }
            _ => {}
        }
        if state.search_bar_contains(&self.config, pos)
            || state.status_bar_contains(&self.config, pos)
        {
            return;
        }
        if left && let Some((_, link)) = &state.mouse.hover_link {
            let url = link.url.clone();
            self.open_url(&url);
            return;
        }
        if left && let Some(divider) = state.divider_at(&self.config, pos) {
            state.mouse.divider_drag = Some(divider);
            return;
        }
        // Clicking a pane focuses it; the click then acts inside it.
        let Some(id) = state.pane_at(&self.config, pos) else {
            return;
        };
        state.focus_pane(id);
        let point = state.cell_at(&self.config, pos);
        // Application mouse mode: forward presses and matching releases.
        if state.reports_mouse(state.term().mode()) {
            state.report(Some(button), MouseAction::Press, point);
            state.mouse.reported_buttons.insert(button);
            // Where a release outside the window is reported.
            state.mouse.last_reported_cell = Some((point.column, point.line));
            return;
        }
        match button {
            Button::Left => start_selection(state, point),
            // Middle click pastes the primary selection, as on X11.
            Button::Middle => {
                if let Some(text) = self.primary_text() {
                    self.paste(state, text);
                }
            }
            _ => {}
        }
    }

    fn mouse_release(
        &mut self,
        state: &mut WindowState,
        button: Button,
        pos: PhysicalPosition<f64>,
    ) {
        if button == Button::Left
            && (state.mouse.tab_drag.take().is_some() || state.mouse.divider_drag.take().is_some())
        {
            return;
        }
        if state.mouse.reported_buttons.remove(button) {
            let point = state.cell_at(&self.config, pos);
            state.report(Some(button), MouseAction::Release, point);
            return;
        }
        if button == Button::Left && state.mouse.selecting {
            self.finish_selection(state);
        }
    }

    /// A click while the actions menu is open: an entry runs its action, a
    /// heading does nothing, anywhere else closes the menu. The menu takes
    /// the click either way.
    fn press_actions_menu(
        &mut self,
        state: &mut WindowState,
        button: Button,
        pos: PhysicalPosition<f64>,
    ) {
        let view = state.menu_view();
        let (x, y) = (pos.x as f32, pos.y as f32);
        let Some(menu) = &state.actions_menu else {
            return;
        };
        if !menu.contains(view, x, y) {
            state.close_actions_menu();
        } else if button == Button::Left
            && let Some(action) = menu.click(view, x, y)
        {
            state.close_actions_menu();
            self.run_action(state, action);
        }
    }

    /// A click on the banner pages through its messages, opens its link or
    /// closes it. Returns whether the click was on the banner.
    fn press_banner(&mut self, state: &WindowState, pos: PhysicalPosition<f64>) -> bool {
        let Some(banner) = &self.banner else {
            return false;
        };
        if !state.banner_contains(&self.config, banner, pos) {
            return false;
        }
        let width = state.window.inner_size().width as f32;
        let metrics = state.ui_metrics();
        let x = pos.x as f32;
        let url = banner
            .url
            .clone()
            .filter(|_| !banner.on_close(x, width, metrics));
        let stays = url.is_none()
            && self
                .banner
                .as_mut()
                .is_some_and(|banner| banner.click(x, width, metrics));
        state.window.request_redraw();
        if !stays && let Some(closed) = self.banner.take() {
            // Closing the update banner means: not for this release.
            if closed.title == update::BANNER
                && let Some(update) = &self.update
            {
                update::dismiss(&update.version);
            }
        }
        if let Some(url) = url {
            self.open_url(&url);
        }
        true
    }

    /// A press on the edges of an undecorated window or on the tab bar.
    /// Returns whether it was one.
    fn press_window_chrome(
        &mut self,
        state: &mut WindowState,
        button: Button,
        pos: PhysicalPosition<f64>,
    ) -> bool {
        if button == Button::Left
            && let Some(direction) = state.resize_edge(pos)
        {
            if !self.synthetic_input {
                let _ = state.window.drag_resize_window(direction);
            }
            return true;
        }
        let hit = state
            .tab_bar(&self.config)
            .and_then(|bar| bar.hit(pos.x as f32, pos.y as f32));
        match (button, hit) {
            (Button::Left, Some(hit)) => {
                self.press_tab_bar(state, hit, pos);
                true
            }
            // Middle click closes a tab, like in browsers and iTerm2.
            (Button::Middle, Some(BarHit::Tab(index) | BarHit::Close(index))) => {
                self.request_close_tab(state, index);
                true
            }
            (Button::Right, Some(BarHit::NewTab)) => {
                self.open_shell_menu(state, true);
                true
            }
            _ => false,
        }
    }

    fn press_tab_bar(&mut self, state: &mut WindowState, hit: BarHit, pos: PhysicalPosition<f64>) {
        match hit {
            BarHit::Tab(index) => {
                state.select_tab(index);
                state.mouse.tab_drag = Some(TabDrag {
                    index,
                    press_x: pos.x,
                    moved: false,
                });
            }
            BarHit::Close(index) => self.request_close_tab(state, index),
            BarHit::NewTab => self.new_tab(state, self.default_launch()),
            BarHit::Empty => {
                // Double click maximizes, like a title bar.
                let now = Instant::now();
                let double = state
                    .mouse
                    .last_bar_click
                    .is_some_and(|t| now.duration_since(t) < MULTI_CLICK_INTERVAL);
                if double {
                    state.mouse.last_bar_click = None;
                    state.toggle_maximized();
                } else {
                    state.mouse.last_bar_click = Some(now);
                    if !self.synthetic_input {
                        let _ = state.window.drag_window();
                    }
                }
            }
            BarHit::Minimize => state.window.set_minimized(true),
            BarHit::Maximize => state.toggle_maximized(),
            BarHit::CloseWindow => self.request_close(state, CloseTarget::Window),
            BarHit::Update => self.open_update(),
        }
    }

    /// The selection drag ended: copy what it selected.
    fn finish_selection(&mut self, state: &mut WindowState) {
        state.mouse.selecting = false;
        state.mouse.autoscroll = None;
        if let Some(text) = state.term().selection_text() {
            if self.config.mouse.copy_on_select {
                self.set_clipboard(text.clone());
            }
            self.set_primary(text);
        }
    }

    /// A button went up while the pointer is outside the window: end drags,
    /// the selection and a reported press without starting anything new.
    fn release_outside(&mut self, state: &mut WindowState, button: Button) {
        if button == Button::Left {
            state.mouse.end_drags();
        }
        if state.mouse.reported_buttons.remove(button)
            && let Some((column, line)) = state.mouse.last_reported_cell
        {
            let point = GridPoint {
                column,
                line,
                right_half: false,
            };
            state.report(Some(button), MouseAction::Release, point);
        }
        if button == Button::Left && state.mouse.selecting {
            self.finish_selection(state);
        }
    }

    pub(super) fn cursor_moved(&mut self, state: &mut WindowState, pos: PhysicalPosition<f64>) {
        state.mouse.position = Some(pos);
        if state.actions_menu.is_some() {
            self.cursor_over_actions_menu(state, pos);
            return;
        }
        state.update_hover_link(&self.config);
        if let Some(divider) = state.mouse.divider_drag.clone() {
            state.drag_divider(&self.config, &divider, pos);
            return;
        }
        let bar = state.tab_bar(&self.config);
        let bar_hit = bar.as_ref().and_then(|b| b.hit(pos.x as f32, pos.y as f32));
        let in_terminal = self.set_cursor_icon(state, pos, bar_hit);
        if bar_hit != state.mouse.hovered_bar {
            state.mouse.hovered_bar = bar_hit;
            state.window.request_redraw();
        }
        if let Some(drag) = state.mouse.tab_drag {
            state.drag_tab(drag, bar.as_ref(), pos);
        } else if state.mouse.selecting {
            state.extend_selection(&self.config, pos);
        } else if in_terminal {
            state.report_motion(&self.config, pos);
        }
    }

    /// The pointer moved while the actions menu is open: select the entry
    /// under it. Nothing else reacts to the pointer meanwhile.
    fn cursor_over_actions_menu(&mut self, state: &mut WindowState, pos: PhysicalPosition<f64>) {
        let view = state.menu_view();
        let (x, y) = (pos.x as f32, pos.y as f32);
        let Some(menu) = state.actions_menu.as_mut() else {
            return;
        };
        let over_entry = menu.click(view, x, y).is_some();
        if menu.hover(view, x, y) {
            state.window.request_redraw();
        }
        state.window.set_cursor(if over_entry {
            CursorIcon::Pointer
        } else {
            CursorIcon::Default
        });
    }

    /// Show the pointer that fits what is under it at `pos`. Returns
    /// whether that is the terminal.
    fn set_cursor_icon(
        &self,
        state: &WindowState,
        pos: PhysicalPosition<f64>,
        bar_hit: Option<BarHit>,
    ) -> bool {
        let divider_icon = state.divider_at(&self.config, pos).map(|d| match d.axis {
            Axis::Vertical => CursorIcon::ColResize,
            Axis::Horizontal => CursorIcon::RowResize,
        });
        let frame_icon = state.resize_edge(pos).map(resize_cursor).or(divider_icon);
        let in_terminal = bar_hit.is_none() && frame_icon.is_none();
        let over_link = state.mouse.hover_link.is_some();
        let over_update = bar_hit == Some(BarHit::Update)
            || matches!(
                state.status_item_at(&self.config, &self.stats, pos),
                Some(StatusItem::Update | StatusItem::Actions | StatusItem::Shell)
            );
        state.window.set_cursor(if over_link || over_update {
            CursorIcon::Pointer
        } else if in_terminal {
            CursorIcon::Text
        } else {
            frame_icon.unwrap_or(CursorIcon::Default)
        });
        in_terminal
    }
}

/// Start a selection at `point`, or extend the current one with Shift.
fn start_selection(state: &mut WindowState, point: GridPoint) {
    let mods = state.modifiers.state();
    if mods.shift_key() && state.term().has_selection() {
        // Shift+click extends the selection, keeping its kind.
        state.term().update_selection(point);
    } else {
        let kind = match state
            .mouse
            .clicks
            .click(Instant::now(), point.column, point.line)
        {
            2 => SelectionKind::Semantic,
            3 => SelectionKind::Lines,
            _ if mods.alt_key() => SelectionKind::Block,
            _ => SelectionKind::Simple,
        };
        state.term().start_selection(kind, point);
    }
    state.mouse.selecting = true;
    state.window.request_redraw();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pastes_with_line_breaks_are_confirmed() {
        assert_eq!(paste_warning("ls -la"), None);
        assert_eq!(
            paste_warning("rm -rf build\n").as_deref(),
            Some("the text has a line break and would run at once; paste again to run it")
        );
        assert_eq!(
            paste_warning("cd /tmp\r\nls\n\nmake\n").as_deref(),
            Some("3 lines would run at once; paste again to run them")
        );
    }

    #[test]
    fn alt_is_meta_outside_macos() {
        if cfg!(target_os = "macos") {
            return;
        }
        let alt = Modifiers::from(ModifiersState::ALT);
        assert!(alt_is_meta(&alt, OptionAsMeta::None));
        assert!(!alt_is_meta(&Modifiers::default(), OptionAsMeta::Both));
    }
}
