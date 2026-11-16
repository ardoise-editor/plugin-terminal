//! The Terminal plugin of SimpleEdit: shells in pseudo-terminals, in a
//! panel under the editor, one tab each. The editor draws their screens and
//! sends back what is typed; the plugin runs them and keeps their history.

mod session;
mod snapshot;
mod text;

use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use simpleedit_plugin::protocol::{Context, InitializeParams, Settings, UiEvent};
use simpleedit_plugin::ui::*;
use simpleedit_plugin::{run, Host, Plugin};

use session::Session;
use text::t;

const PANEL: &str = "terminal";
/// The screen is drawn again at most this often while a shell prints.
const FRAME: Duration = Duration::from_millis(33);

struct Tab {
    title: String,
    session: Arc<Session>,
}

#[derive(Default)]
struct Terminals {
    tabs: Vec<Tab>,
    active: usize,
    started: usize,
    /// What the search bar looks for (none: no bar).
    search: Option<String>,
    french: bool,
    /// The panel is open: its screen is drawn as the shells print.
    shown: bool,
}

impl Terminals {
    fn add(&mut self, dir: &Path) -> Result<(), String> {
        let session = Session::spawn_shell(dir, 24, 80)?;
        self.started += 1;
        let local = t(self.french, "local");
        let title = if self.started == 1 {
            local.to_string()
        } else {
            format!("{local} ({})", self.started)
        };
        self.tabs.push(Tab { title, session });
        self.active = self.tabs.len() - 1;
        Ok(())
    }

    fn close(&mut self, index: usize) {
        if index < self.tabs.len() {
            self.tabs.remove(index);
        }
        self.fit();
    }

    /// Drops the terminals whose shell ended; true if there were any.
    fn prune_ended(&mut self) -> bool {
        let before = self.tabs.len();
        self.tabs.retain(|tab| tab.session.is_alive());
        self.fit();
        self.tabs.len() != before
    }

    fn fit(&mut self) {
        if self.active >= self.tabs.len() {
            self.active = self.tabs.len().saturating_sub(1);
        }
    }

    fn active(&self) -> Option<&Arc<Session>> {
        self.tabs.get(self.active).map(|t| &t.session)
    }

    fn view(&self) -> Node {
        let f = self.french;
        let mut bar: Vec<Node> = self
            .tabs
            .iter()
            .enumerate()
            .map(|(i, tab)| {
                button(tab.title.clone(), format!("tab:{i}")).button_style(if i == self.active {
                    ButtonStyle::Outline
                } else {
                    ButtonStyle::Ghost
                })
            })
            .collect();
        bar.push(button("+", "new").button_style(ButtonStyle::Ghost));
        if !self.tabs.is_empty() {
            bar.push(button(t(f, "close"), "close").button_style(ButtonStyle::Ghost));
            bar.push(button(t(f, "find"), "search").button_style(ButtonStyle::Ghost));
        }
        let mut body = vec![row(bar).spacing(4.0)];
        if let Some(query) = &self.search {
            body.push(
                row([
                    input("query", query, t(f, "find_hint")).on_submit("older"),
                    button("↑", "older").button_style(ButtonStyle::Ghost),
                    button("↓", "newer").button_style(ButtonStyle::Ghost),
                ])
                .spacing(4.0),
            );
        }
        match self.active() {
            Some(session) => {
                let highlight = self.search.clone().unwrap_or_default();
                let dir = session.start_dir();
                if let Some(screen) =
                    session.with_screen(|s| snapshot::screen("screen", s, &dir, &highlight))
                {
                    body.push(Node::Screen(Box::new(screen)));
                }
            }
            None => body.push(text(t(f, "none")).style(TextStyle::Muted)),
        }
        column(body).spacing(4.0)
    }
}

struct Terminal {
    shared: Arc<Mutex<Terminals>>,
    /// Where a new shell starts: the folder of the file in front, else the
    /// first project, else the home folder.
    active_file: Option<PathBuf>,
    workspace: Vec<PathBuf>,
}

fn home() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/"))
}

impl Terminal {
    fn shared(&self) -> std::sync::MutexGuard<'_, Terminals> {
        self.shared.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn start_dir(&self) -> PathBuf {
        self.workspace
            .iter()
            .find(|root| {
                self.active_file
                    .as_ref()
                    .is_some_and(|f| f.starts_with(root))
            })
            .or_else(|| self.workspace.first())
            .cloned()
            .unwrap_or_else(home)
    }

    fn new_tab(&self, host: &Host, dir: &Path) {
        let mut s = self.shared();
        if let Err(e) = s.add(dir) {
            host.error(format!("{}: {e}", t(s.french, "failed")));
        }
    }

    fn draw(&self, host: &Host) {
        let s = self.shared();
        host.render(PANEL, s.view());
    }
}

impl Plugin for Terminal {
    fn initialize(&mut self, host: &Host, params: &InitializeParams) {
        self.workspace = params.workspace.clone();
        self.shared().french = params.settings.locale.starts_with("fr");
        // The screen follows what the shells print, a frame at a time.
        let (shared, host) = (self.shared.clone(), host.clone());
        std::thread::spawn(move || {
            let mut seen = session::OUTPUT.load(Ordering::Relaxed);
            loop {
                std::thread::sleep(FRAME);
                let now = session::OUTPUT.load(Ordering::Relaxed);
                if now == seen {
                    continue;
                }
                seen = now;
                let mut s = shared.lock().unwrap_or_else(|e| e.into_inner());
                s.prune_ended();
                if s.shown {
                    host.render(PANEL, s.view());
                }
            }
        });
    }

    fn command(&mut self, host: &Host, command: &str, context: &Context) {
        if !context.workspace.is_empty() {
            self.workspace = context.workspace.clone();
        }
        match command {
            "terminal.toggle" | "terminal.new" => {
                let empty = self.shared().tabs.is_empty();
                if empty || command == "terminal.new" {
                    let dir = self.start_dir();
                    self.new_tab(host, &dir);
                }
                host.show_panel(PANEL);
                self.draw(host);
            }
            "terminal.here" => {
                let dir = match &context.file {
                    Some(p) if p.is_dir() => p.clone(),
                    Some(p) => p.parent().map(Path::to_path_buf).unwrap_or_else(home),
                    None => self.start_dir(),
                };
                self.new_tab(host, &dir);
                host.show_panel(PANEL);
                self.draw(host);
            }
            _ => {}
        }
    }

    fn panel_opened(&mut self, host: &Host, panel: &str) {
        if panel != PANEL {
            return;
        }
        let empty = {
            let mut s = self.shared();
            s.shown = true;
            s.tabs.is_empty()
        };
        if empty {
            let dir = self.start_dir();
            self.new_tab(host, &dir);
        }
        self.draw(host);
    }

    fn panel_closed(&mut self, _: &Host, panel: &str) {
        if panel == PANEL {
            self.shared().shown = false;
        }
    }

    fn ui_event(&mut self, host: &Host, event: &UiEvent) {
        if event.action == "screen" {
            let s = self.shared();
            let (Some(session), Some(input)) = (s.active(), ScreenInput::from_event(event)) else {
                return;
            };
            match input {
                ScreenInput::Input { bytes } => session.write(&bytes),
                ScreenInput::Resize { rows, cols } => session.resize(rows, cols),
                ScreenInput::Scroll { lines } => session.scroll(lines),
            }
            // What changes the screen by itself shows at the next frame.
            session::OUTPUT.fetch_add(1, Ordering::Relaxed);
            return;
        }
        match event.action.as_str() {
            "new" => {
                let dir = self.start_dir();
                self.new_tab(host, &dir);
            }
            "close" => {
                let mut s = self.shared();
                let active = s.active;
                s.close(active);
            }
            "search" => {
                let mut s = self.shared();
                s.search = if s.search.is_some() {
                    None
                } else {
                    Some(String::new())
                };
                if let Some(session) = s.active() {
                    session.forget_found();
                }
            }
            "query" => {
                let mut s = self.shared();
                s.search = Some(event.text().unwrap_or_default().to_string());
                if let Some(session) = s.active() {
                    session.forget_found();
                }
            }
            "older" | "newer" => {
                let s = self.shared();
                let query = s.search.clone().unwrap_or_default();
                if let Some(session) = s.active() {
                    if !session.find(&query, event.action == "older") {
                        host.notify(t(s.french, "not_found"));
                    }
                }
            }
            action => {
                let Some(i) = action
                    .strip_prefix("tab:")
                    .and_then(|i| i.parse::<usize>().ok())
                else {
                    return;
                };
                let mut s = self.shared();
                if i < s.tabs.len() {
                    s.active = i;
                }
            }
        }
        self.draw(host);
    }

    fn active_file_changed(&mut self, _: &Host, path: Option<&Path>) {
        if let Some(path) = path {
            self.active_file = Some(path.to_path_buf());
        }
    }

    fn settings_changed(&mut self, host: &Host, settings: &Settings) {
        self.shared().french = settings.locale.starts_with("fr");
        self.draw(host);
    }

    fn shutdown(&mut self, _: &Host) {
        // Dropping the sessions ends the shells and what they started.
        self.shared().tabs.clear();
    }
}

fn main() -> std::io::Result<()> {
    run(Terminal {
        shared: Arc::default(),
        active_file: None,
        workspace: Vec::new(),
    })
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn a_tab_holds_a_shell_and_its_screen_shows_in_the_panel() {
        let mut s = Terminals::default();
        let dir = tempfile::tempdir().unwrap();
        s.add(dir.path()).unwrap();
        s.add(dir.path()).unwrap();
        assert_eq!(s.tabs[1].title, "Local (2)");
        assert_eq!(s.active, 1);
        let page = serde_json::to_value(s.view()).unwrap();
        let text = page.to_string();
        assert!(text.contains("\"type\":\"screen\"") && text.contains("tab:1"));
        s.close(1);
        assert_eq!((s.tabs.len(), s.active), (1, 0));
        s.close(0);
        assert!(serde_json::to_string(&s.view())
            .unwrap()
            .contains("No terminal"));
    }
}
