//! A shell in a pseudo-terminal, its output parsed by `vt100` into a grid
//! of cells.

use std::io::{Read, Write};
use std::num::NonZeroU16;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Weak};

use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtySize};

pub const SCROLLBACK: usize = 5_000;

/// Counts what the shells print (and their ends): the repaint waits for it.
pub static OUTPUT: AtomicU64 = AtomicU64::new(0);

/// The replies to the queries found in what a program wrote: the position of
/// the cursor (`CSI 6 n`), the status (`CSI 5 n`) and the kind of terminal
/// (`CSI c`, `CSI > c`).
fn queries_answered(chunk: &[u8], screen: &vt100::Screen) -> Vec<u8> {
    let mut reply = Vec::new();
    let has = |needle: &[u8]| chunk.windows(needle.len()).any(|w| w == needle);
    if has(b"\x1b[6n") {
        let (row, col) = screen.cursor_position();
        reply.extend(format!("\x1b[{};{}R", row + 1, col + 1).bytes());
    }
    if has(b"\x1b[5n") {
        reply.extend(b"\x1b[0n");
    }
    if has(b"\x1b[c") || has(b"\x1b[0c") {
        reply.extend(b"\x1b[?1;2c");
    }
    if has(b"\x1b[>c") || has(b"\x1b[>0c") {
        reply.extend(b"\x1b[>0;0;0c");
    }
    reply
}

fn nonzero(n: u16) -> NonZeroU16 {
    NonZeroU16::new(n.max(1)).unwrap_or(NonZeroU16::MIN)
}

// ─── The shell ──────────────────────────────────────────────────────────────

/// A shell running in a pseudo-terminal.
pub struct Session {
    parser: Mutex<vt100::Parser>,
    writer: Mutex<Box<dyn Write + Send>>,
    master: Mutex<Box<dyn MasterPty + Send>>,
    child: Mutex<Box<dyn Child + Send + Sync>>,
    alive: AtomicBool,
    size: Mutex<(u16, u16)>,
    /// Where the shell was started: what a relative path of its output is
    /// from.
    cwd: Mutex<PathBuf>,
    /// The line the last search found (counted from the bottom).
    found: Mutex<Option<usize>>,
}

impl Session {
    /// The user's shell, started in `cwd`.
    pub fn spawn_shell(cwd: &Path, rows: u16, cols: u16) -> Result<Arc<Session>, String> {
        let shell = std::env::var("SHELL")
            .ok()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "/bin/sh".to_string());
        let mut command = CommandBuilder::new(shell);
        // macOS terminals start login shells, which is what sets the PATH.
        if cfg!(target_os = "macos") {
            command.arg("-l");
        }
        command.cwd(cwd);
        let session = Self::spawn_command(command, rows, cols)?;
        if let Ok(mut dir) = session.cwd.lock() {
            *dir = cwd.to_path_buf();
        }
        Ok(session)
    }

    /// The folder the shell was started in.
    pub fn start_dir(&self) -> PathBuf {
        self.cwd.lock().map(|d| d.clone()).unwrap_or_default()
    }

    pub fn spawn_command(
        mut command: CommandBuilder,
        rows: u16,
        cols: u16,
    ) -> Result<Arc<Session>, String> {
        command.env("TERM", "xterm-256color");
        command.env("COLORTERM", "truecolor");
        let pair = native_pty_system()
            .openpty(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|e| e.to_string())?;
        let child = pair
            .slave
            .spawn_command(command)
            .map_err(|e| e.to_string())?;
        drop(pair.slave);
        let mut reader = pair.master.try_clone_reader().map_err(|e| e.to_string())?;
        let writer = pair.master.take_writer().map_err(|e| e.to_string())?;

        let session = Arc::new(Session {
            parser: Mutex::new(vt100::Parser::new(nonzero(rows), nonzero(cols), SCROLLBACK)),
            writer: Mutex::new(writer),
            master: Mutex::new(pair.master),
            child: Mutex::new(child),
            alive: AtomicBool::new(true),
            size: Mutex::new((rows, cols)),
            cwd: Mutex::new(PathBuf::new()),
            found: Mutex::new(None),
        });

        // The thread holds only a weak reference: dropping the session kills
        // the shell, which ends the read.
        let weak: Weak<Session> = Arc::downgrade(&session);
        std::thread::spawn(move || {
            let mut buffer = [0u8; 8192];
            loop {
                match reader.read(&mut buffer) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        let Some(session) = weak.upgrade() else { break };
                        let reply = match session.parser.lock() {
                            Ok(mut parser) => {
                                parser.process(&buffer[..n]);
                                queries_answered(&buffer[..n], parser.screen())
                            }
                            Err(_) => Vec::new(),
                        };
                        // What a program asks of the terminal (where the
                        // cursor is, what the terminal is) is answered, or
                        // it waits for a reply that never comes.
                        if !reply.is_empty() {
                            session.write(&reply);
                        }
                        OUTPUT.fetch_add(1, Ordering::Relaxed);
                    }
                }
            }
            if let Some(session) = weak.upgrade() {
                session.alive.store(false, Ordering::Relaxed);
            }
            OUTPUT.fetch_add(1, Ordering::Relaxed);
        });
        Ok(session)
    }

    /// Sends bytes to the shell, as typed.
    pub fn write(&self, bytes: &[u8]) {
        if let Ok(mut writer) = self.writer.lock() {
            let _ = writer.write_all(bytes);
            let _ = writer.flush();
        }
        // Typing brings the view back to the live screen.
        self.scroll_to_bottom();
    }

    pub fn is_alive(&self) -> bool {
        self.alive.load(Ordering::Relaxed)
    }

    /// Resizes the pseudo-terminal and the screen, if the size changed.
    pub fn resize(&self, rows: u16, cols: u16) {
        let (rows, cols) = (rows.max(1), cols.max(2));
        let Ok(mut size) = self.size.lock() else {
            return;
        };
        if *size == (rows, cols) {
            return;
        }
        *size = (rows, cols);
        if let Ok(master) = self.master.lock() {
            let _ = master.resize(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            });
        }
        if let Ok(mut parser) = self.parser.lock() {
            parser.set_size(nonzero(rows), nonzero(cols));
        }
    }

    /// Reads the screen.
    pub fn with_screen<R>(&self, f: impl FnOnce(&vt100::Screen) -> R) -> Option<R> {
        self.parser.lock().ok().map(|parser| f(parser.screen()))
    }

    /// Scrolls the history: positive goes up, towards older lines.
    /// Looks for `query` (whatever the case) in the screen and what has gone
    /// above it, before the last place found (`older`) or after it, and
    /// scrolls to it. Whether there was one.
    pub fn find(&self, query: &str, older: bool) -> bool {
        let query = query.to_lowercase();
        if query.is_empty() {
            return false;
        }
        let Ok(mut parser) = self.parser.lock() else {
            return false;
        };
        let screen = parser.screen_mut();
        let (rows, cols) = (screen.size().0.get() as usize, screen.size().1.get());
        let current = screen.scrollback();
        // Every line that holds it, as its place counted from the bottom.
        let mut lines: Vec<usize> = Vec::new();
        let mut offset = 0;
        loop {
            screen.set_scrollback(offset);
            let at = screen.scrollback();
            for (r, line) in screen.rows(0, cols).enumerate() {
                if line.to_lowercase().contains(&query) {
                    lines.push(at + rows - 1 - r);
                }
            }
            if at < offset || offset >= SCROLLBACK + rows {
                break;
            }
            offset += rows;
        }
        lines.sort_unstable();
        lines.dedup();
        let last = self.found.lock().ok().and_then(|f| *f);
        let from = last.map_or(current as isize - 1, |l| l as isize);
        let target = if older {
            lines.iter().copied().find(|l| *l as isize > from)
        } else {
            lines.iter().rev().copied().find(|l| (*l as isize) < from)
        };
        match target {
            Some(line) => {
                if let Ok(mut f) = self.found.lock() {
                    *f = Some(line);
                }
                screen.set_scrollback(line.saturating_sub(rows / 2));
                true
            }
            None => {
                screen.set_scrollback(current);
                false
            }
        }
    }

    /// The next search starts from the screen again.
    pub fn forget_found(&self) {
        if let Ok(mut f) = self.found.lock() {
            *f = None;
        }
    }

    pub fn scroll(&self, lines: i32) {
        if let Ok(mut parser) = self.parser.lock() {
            let screen = parser.screen_mut();
            let at = screen.scrollback() as i64 + lines as i64;
            screen.set_scrollback(at.max(0) as usize);
        }
    }

    fn scroll_to_bottom(&self) {
        if let Ok(mut parser) = self.parser.lock() {
            if parser.screen().scrollback() > 0 {
                parser.screen_mut().set_scrollback(0);
            }
        }
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        if let Ok(mut child) = self.child.lock() {
            // The shell leads a group of its own: what it started (a dev
            // server) is asked to end too, and forced after a delay, not
            // left orphaned.
            #[cfg(unix)]
            let group = child.process_id().map(|pid| pid as i32);
            #[cfg(unix)]
            if let Some(id) = group {
                // SAFETY: a signal to a process group has no memory effect.
                unsafe { libc::killpg(id, libc::SIGTERM) };
            }
            let _ = child.kill();
            // Reaped: a dead shell is not left a zombie.
            let _ = child.wait();
            #[cfg(unix)]
            if let Some(id) = group {
                std::thread::spawn(move || {
                    std::thread::sleep(std::time::Duration::from_secs(2));
                    // SAFETY: as above; nothing happens if the group is gone.
                    unsafe { libc::killpg(id, libc::SIGKILL) };
                });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn closing_a_terminal_ends_what_its_shell_started() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("pid");
        let mut command = CommandBuilder::new("sh");
        command.arg("-c");
        command.arg(format!("sleep 300 & echo $! > '{}'; wait", file.display()));
        let session = Session::spawn_command(command, 24, 80).unwrap();
        let wait_for = |what: &dyn Fn() -> bool| {
            for _ in 0..100 {
                if what() {
                    return true;
                }
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            false
        };
        let pid_of = || {
            std::fs::read_to_string(&file)
                .ok()
                .and_then(|t| t.trim().parse::<i32>().ok())
        };
        assert!(wait_for(&|| pid_of().is_some()), "the shell started it");
        let pid = pid_of().unwrap();
        // SAFETY: signal 0 only tests that the process exists.
        let alive = || unsafe { libc::kill(pid, 0) } == 0;
        assert!(alive());
        drop(session);
        assert!(wait_for(&|| !alive()), "it did not outlive the terminal");
    }

    fn wait_for(session: &Session, needle: &str) -> bool {
        for _ in 0..100 {
            let found = session
                .with_screen(|s| s.contents().contains(needle))
                .unwrap_or(false);
            if found {
                return true;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        false
    }

    /// A real shell in a real pseudo-terminal: what is typed comes back
    /// printed, a resize reaches the shell, and the shell's end is noticed.
    #[cfg(unix)]
    #[test]
    fn a_shell_runs_in_the_session() {
        let mut command = CommandBuilder::new("sh");
        command.env("PS1", "$ ");
        let session = Session::spawn_command(command, 24, 80).expect("a shell");
        assert!(session.is_alive());
        session.write(b"echo hello-from-the-pty\n");
        assert!(wait_for(&session, "hello-from-the-pty"));

        session.resize(10, 40);
        session.write(b"stty size\n");
        assert!(wait_for(&session, "10 40"));

        session.write(b"exit\n");
        for _ in 0..100 {
            if !session.is_alive() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        assert!(!session.is_alive());
    }

    /// A program that asks where the cursor is gets the answer: the shell
    /// reads it from its input, and prints it.
    #[cfg(unix)]
    #[test]
    fn the_position_of_the_cursor_is_answered_to_a_program() {
        let mut command = CommandBuilder::new("bash");
        command.env("PS1", "$ ");
        let Ok(session) = Session::spawn_command(command, 24, 80) else {
            return;
        };
        session.write(b"printf '\\033[6n'; read -s -t 3 -d R reply; echo \"answer=${reply#*[}\"\n");
        assert!(wait_for(&session, "answer="));
        assert!(wait_for(&session, ";"), "a row and a column");
        session.write(b"exit\n");
    }

    #[test]
    fn the_queries_of_a_program_have_answers() {
        let mut parser = vt100::Parser::new(nonzero(24), nonzero(80), 0);
        parser.process(b"abc");
        let reply = queries_answered(b"\x1b[6n\x1b[c", parser.screen());
        assert_eq!(reply, b"\x1b[1;4R\x1b[?1;2c");
        assert!(queries_answered(b"plain", parser.screen()).is_empty());
    }

    #[test]
    fn a_search_goes_back_through_the_history_and_forth_again() {
        let mut command = CommandBuilder::new("sh");
        command.env("PS1", "$ ");
        let Ok(session) = Session::spawn_command(command, 5, 40) else {
            return;
        };
        session.write(
            b"echo needle-$((0+1)); seq 1 30; echo needle-$((1+1)); seq 31 60; echo end-$((5+5))\n",
        );
        assert!(wait_for(&session, "end-10"));
        // From the bottom: the last needle, then the one before, then none.
        assert!(session.find("NEEDLE-2", true));
        let first = session.with_screen(|s| s.contents()).unwrap();
        assert!(first.contains("needle-2"), "{first}");
        assert!(session.find("needle-1", true));
        let second = session.with_screen(|s| s.contents()).unwrap();
        assert!(second.contains("needle-1"), "{second}");
        assert!(!session.find("needle-1", true));
        // And forth again.
        assert!(session.find("needle-2", false));
        let back = session.with_screen(|s| s.contents()).unwrap();
        assert!(back.contains("needle-2"), "{back}");
        assert!(!session.find("absent", true));
    }
}
