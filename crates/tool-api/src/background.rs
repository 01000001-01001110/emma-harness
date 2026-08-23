//! Work that outlives the call that started it.
//!
//! Every tool so far answers inside its own `invoke`: the call starts something,
//! waits for it, and returns what happened. That shape is why cancellation could
//! be added by racing `invoke` against the interrupt and *dropping the future* —
//! dropping is the mechanism, and it is exactly the opposite of outliving.
//!
//! A background command needs the other thing. `Bash` spawns, returns an id, and
//! the child keeps running with nobody awaiting it; a later `BashOutput` call
//! asks what has happened since. So something has to hold the child, and it
//! cannot be the call.
//!
//! **What this deliberately is not.** Not a scheduler, not a queue, and not a
//! supervisor. It holds handles, keys them by session, and lets a later call
//! find one. Every policy question — how many, how long, what happens on exit —
//! is answered by the caller or by an explicit rule below, never by inference
//! here.
//!
//! ## The three rules this module is written around
//!
//! 1. **A dead task is still findable.** A handle survives its child's exit, so
//!    `BashOutput` on a finished task returns its tail and its status rather
//!    than "no such task". "It finished" and "it never existed" are different
//!    answers and the model can act on the difference — the same rule
//!    `tools/fs`'s grep learned when a file it could not open was reported as
//!    containing no matches.
//! 2. **Output is capped and the cap is named.** An unbounded buffer behind a
//!    command nobody is reading is a memory leak with a friendly face. The cap
//!    is a constant here, and `drained` reports what was dropped so the reader
//!    is never told a truncated tail is the whole of it.
//! 3. **Killing kills the child, and says what it did not kill.** Emma does not
//!    reap process trees — that was ruled out in `notes/plans/process-lifetime.md`
//!    when the owner ruled that a hook may deliberately daemonize. The same rule
//!    applies here, so `kill` is honest about its blast radius rather than
//!    implying a guarantee it does not provide.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

/// How much of one task's output is kept in memory.
///
/// A background command is one nobody is waiting for, which is exactly the
/// command most likely to produce megabytes while the model does something else.
/// The oldest bytes go first: the tail is what a reader wants, and a head that
/// scrolled away hours ago is not worth the resident memory.
pub const MAX_TASK_OUTPUT_BYTES: usize = 256 * 1024;

/// What a background task is doing, or what it did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TaskState {
    Running,
    /// The child exited on its own. `None` when the platform reported no code,
    /// which on unix means a signal — reported as its own thing rather than
    /// flattened to a number the caller would read as an exit status.
    Exited(Option<i32>),
    /// `kill` was called and the child was signalled.
    Killed,
    /// The task could not be run, or the wait itself failed.
    Failed(String),
}

impl TaskState {
    pub fn finished(&self) -> bool {
        !matches!(self, TaskState::Running)
    }
}

/// One background task's shared state.
///
/// Every field is behind the same mutex rather than one mutex per field, because
/// a reader that saw `Running` alongside the final output — or a final status
/// alongside a stale tail — would report a coherent-looking lie. One lock means
/// one consistent answer.
struct Shared {
    state: TaskState,
    /// How to stop this task, taken when it is used.
    ///
    /// **Under the same lock as `state`, and that is the whole point.** It used
    /// to have a mutex of its own, so `kill` could take the killer while another
    /// thread was writing the exit status — and `Task::kill` returned `true` for
    /// any *registered* killer even when the child had already exited on its
    /// own, then stamped `Killed` over the real status. A caller was told it had
    /// stopped something it had not, and the true exit code was destroyed on the
    /// way past.
    ///
    /// Found by the agent building `KillShell`, which worked around it by
    /// checking `state()` before calling `kill()` — a fix that cannot be
    /// complete at that layer, because the check and the call are two locks and
    /// a task can exit between them.
    killer: Option<Killer>,
    output: Vec<u8>,
    /// Bytes discarded from the front to stay under the cap. Reported, never
    /// silently absorbed.
    dropped: u64,
    /// How far a reader has consumed. `BashOutput` returns what is new, because
    /// a poll loop that re-reads the whole buffer every time makes the model pay
    /// for the same bytes repeatedly.
    read_to: usize,
}

/// How to stop one task. Boxed because the caller owns the mechanism — a
/// `tokio` child handle, an abort handle, or in a test a flag — and this module
/// deliberately knows none of them.
///
/// Not `Debug`, which is why `Shared` and `Task` write theirs out by hand.
type Killer = Box<dyn FnOnce() + Send>;

/// A handle on one background task. Cloning is cheap and shares the state.
#[derive(Clone)]
pub struct Task {
    pub id: String,
    /// What was run, kept for `/tasks` and the status line. A task nobody can
    /// identify is a task nobody will kill.
    pub label: String,
    pub session_id: String,
    shared: Arc<Mutex<Shared>>,
}

/// What a reader gets back, and everything it needs to be honest about it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskRead {
    pub state: TaskState,
    /// Output not previously returned to a reader.
    pub new_output: String,
    /// Bytes dropped from the front of the buffer to stay under the cap, across
    /// the whole life of the task.
    pub dropped: u64,
}

impl std::fmt::Debug for Shared {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Shared")
            .field("state", &self.state)
            .field("killer", &self.killer.is_some())
            .field("output", &self.output.len())
            .field("dropped", &self.dropped)
            .field("read_to", &self.read_to)
            .finish()
    }
}

impl std::fmt::Debug for Task {
    /// Written out rather than derived, because the killer is a boxed closure
    /// and has no `Debug`. Wrapping it in a newtype purely to satisfy a derive
    /// would add a type that means nothing.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Task")
            .field("id", &self.id)
            .field("label", &self.label)
            .field("session_id", &self.session_id)
            .field("state", &self.state())
            .finish_non_exhaustive()
    }
}

impl Task {
    /// Append bytes the child produced.
    ///
    /// Takes bytes rather than a `String` because a child's stdout is not
    /// guaranteed to be UTF-8 and a partial read can split a character in half.
    /// The conversion happens once, at read time, over a whole buffer.
    pub fn push(&self, bytes: &[u8]) {
        let mut s = self.shared.lock().unwrap_or_else(|e| e.into_inner());
        s.output.extend_from_slice(bytes);
        if s.output.len() > MAX_TASK_OUTPUT_BYTES {
            let excess = s.output.len() - MAX_TASK_OUTPUT_BYTES;
            s.output.drain(..excess);
            s.dropped += excess as u64;
            // The read cursor moves with the bytes it pointed at. Without this a
            // reader that had consumed 10 bytes would, after a drain of 20, be
            // pointing past the front of a buffer that no longer contains what
            // it read — and would be handed a slice it had already seen.
            s.read_to = s.read_to.saturating_sub(excess);
        }
    }

    pub fn set_state(&self, state: TaskState) {
        let mut s = self.shared.lock().unwrap_or_else(|e| e.into_inner());
        s.state = state;
    }

    pub fn state(&self) -> TaskState {
        self.shared
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .state
            .clone()
    }

    /// Everything new since the last call, plus the state at the moment the
    /// output was taken.
    ///
    /// **State is read under the same lock as the output, and that ordering is
    /// the point.** Reading them separately allows the interleaving where a task
    /// exits between the two reads, so the caller is handed the final output
    /// labelled `Running` — and a polling caller believes there is more coming
    /// when there never will be, which is a hang rather than a wrong number.
    pub fn read_new(&self) -> TaskRead {
        let mut s = self.shared.lock().unwrap_or_else(|e| e.into_inner());
        let from = s.read_to.min(s.output.len());
        let new_output = String::from_utf8_lossy(&s.output[from..]).into_owned();
        s.read_to = s.output.len();
        TaskRead {
            state: s.state.clone(),
            new_output,
            dropped: s.dropped,
        }
    }

    /// Everything held, without moving the cursor. For `/tasks` and any reader
    /// that wants to look without consuming.
    pub fn peek_all(&self) -> String {
        let s = self.shared.lock().unwrap_or_else(|e| e.into_inner());
        String::from_utf8_lossy(&s.output).into_owned()
    }

    /// Register how to stop this task. Called once, by whoever spawned it.
    pub fn on_kill(&self, f: impl FnOnce() + Send + 'static) {
        let mut s = self.shared.lock().unwrap_or_else(|e| e.into_inner());
        s.killer = Some(Box::new(f));
    }

    /// Stop the task, if it is running and if a killer was registered.
    ///
    /// Returns whether anything was actually signalled, so the caller can tell
    /// "I stopped it" from "it had already finished" — two different sentences
    /// for the model, and reporting the second as the first is the kind of
    /// plausible success this project treats as a defect.
    ///
    /// **What it does not do is kill descendants.** A shell that spawned a
    /// server keeps that server. Emma does not reap process trees, because the
    /// owner ruled that a deliberately daemonizing child is a supported use, and
    /// nothing here can distinguish one of those from an orphaned mess. The
    /// caller says so out loud rather than implying a guarantee this does not
    /// provide.
    pub fn kill(&self) -> bool {
        let mut s = self.shared.lock().unwrap_or_else(|e| e.into_inner());
        // **Finished first, under this lock.** A task that exited on its own
        // still holds a registered killer, and firing it would signal a pid that
        // is gone — or, worse, one the operating system has since handed to
        // somebody else — and then overwrite the real exit status with `Killed`.
        // Checking outside the lock is not enough: the task can exit between the
        // check and the call, which is exactly the window a caller cannot close
        // from outside.
        if s.state.finished() {
            return false;
        }
        match s.killer.take() {
            Some(f) => {
                f();
                s.state = TaskState::Killed;
                true
            }
            None => false,
        }
    }
}

/// Every background task, keyed by id, scoped by session.
///
/// Cloning shares the same registry — it is an `Arc` inside, and it is passed by
/// value in `ToolCtx` so a tool never has to reach for a global to find it. A
/// process-global would have been fewer lines and is the shape this codebase has
/// already been bitten by elsewhere: ambient state that a test cannot isolate and
/// a second session cannot avoid.
#[derive(Clone, Default)]
pub struct Registry {
    tasks: Arc<Mutex<HashMap<String, Task>>>,
    next: Arc<Mutex<u64>>,
}

impl std::fmt::Debug for Registry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let n = self.tasks.lock().map(|t| t.len()).unwrap_or(0);
        f.debug_struct("Registry").field("tasks", &n).finish()
    }
}

impl Registry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Make a task and put it in the registry.
    ///
    /// The id is short and sequential rather than a uuid, because a model has to
    /// type it back and a human has to read it in `/tasks`. Sequential is also
    /// what makes a stale id *detectable* rather than merely absent: `bash_3` in
    /// a session that has started one task is a mistake worth a clear message.
    pub fn spawn(&self, session_id: &str, label: impl Into<String>) -> Task {
        let id = {
            let mut n = self.next.lock().unwrap_or_else(|e| e.into_inner());
            *n += 1;
            format!("bash_{n}")
        };
        let task = Task {
            id: id.clone(),
            label: label.into(),
            session_id: session_id.to_string(),
            shared: Arc::new(Mutex::new(Shared {
                state: TaskState::Running,
                killer: None,
                output: Vec::new(),
                dropped: 0,
                read_to: 0,
            })),
        };
        self.tasks
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(id, task.clone());
        task
    }

    /// Find a task belonging to this session.
    ///
    /// Session-scoped on purpose: an id from another session is reported as not
    /// found rather than silently answered, because two sessions sharing one
    /// process must not be able to read or kill each other's work by guessing a
    /// short id — and the ids are short and sequential, so guessing is trivial.
    pub fn get(&self, session_id: &str, id: &str) -> Option<Task> {
        self.tasks
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(id)
            .filter(|t| t.session_id == session_id)
            .cloned()
    }

    /// Every task for one session, oldest first.
    pub fn list(&self, session_id: &str) -> Vec<Task> {
        let tasks = self.tasks.lock().unwrap_or_else(|e| e.into_inner());
        let mut v: Vec<Task> = tasks
            .values()
            .filter(|t| t.session_id == session_id)
            .cloned()
            .collect();
        v.sort_by(|a, b| a.id.cmp(&b.id));
        v
    }

    /// Tasks still running, for the status line and for the exit path.
    pub fn running(&self, session_id: &str) -> Vec<Task> {
        self.list(session_id)
            .into_iter()
            .filter(|t| !t.state().finished())
            .collect()
    }

    /// Stop everything still running in this session, returning what was
    /// actually signalled.
    ///
    /// **The exit path calls this, and then says what it did.** A session that
    /// ends leaving children alive is the shape of leak this project has already
    /// paid for once; a session that kills them silently is the shape of
    /// surprise it has paid for twice. Neither is acceptable, so the caller gets
    /// the list back and is expected to name it.
    pub fn kill_all(&self, session_id: &str) -> Vec<Task> {
        self.running(session_id)
            .into_iter()
            .filter(|t| t.kill())
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_finished_task_is_still_findable_with_its_output() {
        // "It finished" and "it never existed" are different answers, and a
        // registry that forgets a task the moment it exits can only give the
        // second. The model polls after the work is done far more often than
        // during it.
        let r = Registry::new();
        let t = r.spawn("s1", "cargo build");
        t.push(b"done\n");
        t.set_state(TaskState::Exited(Some(0)));

        let found = r.get("s1", &t.id).expect("a finished task vanished");
        let read = found.read_new();
        assert_eq!(read.state, TaskState::Exited(Some(0)));
        assert_eq!(read.new_output, "done\n");
    }

    #[test]
    fn a_reader_gets_what_is_new_and_never_the_same_bytes_twice() {
        let r = Registry::new();
        let t = r.spawn("s1", "tail -f log");
        t.push(b"one\n");
        assert_eq!(t.read_new().new_output, "one\n");
        // Nothing new: an empty string, not a repeat.
        assert_eq!(t.read_new().new_output, "");
        t.push(b"two\n");
        assert_eq!(t.read_new().new_output, "two\n");
    }

    #[test]
    fn output_over_the_cap_drops_the_oldest_and_says_how_much() {
        // The failure this guards is a truncation nobody is told about. A tail
        // that silently became "the whole output" is a wrong answer the reader
        // cannot detect, which is the class this repository treats as worse
        // than a loud failure.
        let r = Registry::new();
        let t = r.spawn("s1", "noisy");
        let chunk = vec![b'x'; MAX_TASK_OUTPUT_BYTES];
        t.push(&chunk);
        t.push(b"tail");

        let read = t.read_new();
        assert_eq!(read.dropped, 4, "the drop was not counted");
        assert!(
            read.new_output.ends_with("tail"),
            "the newest bytes were the ones discarded"
        );
        assert_eq!(read.new_output.len(), MAX_TASK_OUTPUT_BYTES);
    }

    #[test]
    fn the_read_cursor_survives_a_drop_from_the_front() {
        // The subtle one. A reader that had consumed some bytes, followed by a
        // drain that removed more than it had read, must not be handed bytes it
        // has already seen — the cursor indexes a buffer whose front moved.
        let r = Registry::new();
        let t = r.spawn("s1", "noisy");
        t.push(b"aaaa");
        assert_eq!(t.read_new().new_output, "aaaa");

        t.push(&vec![b'b'; MAX_TASK_OUTPUT_BYTES]);
        let read = t.read_new();
        assert!(
            !read.new_output.contains('a'),
            "a reader was handed bytes it had already consumed"
        );
        assert_eq!(read.dropped, 4);
    }

    #[test]
    fn one_session_cannot_reach_another_sessions_task() {
        // The ids are short and sequential, so guessing one is trivial. Scoping
        // the lookup is what stops a guess from working.
        let r = Registry::new();
        let t = r.spawn("s1", "secret work");
        assert!(r.get("s2", &t.id).is_none(), "a session id was not checked");
        assert!(r.get("s1", &t.id).is_some());
        assert!(r.list("s2").is_empty());
    }

    #[test]
    fn killing_says_whether_it_actually_stopped_something() {
        use std::sync::atomic::{AtomicBool, Ordering};
        let r = Registry::new();
        let t = r.spawn("s1", "sleep 100");
        let fired = Arc::new(AtomicBool::new(false));
        let f = fired.clone();
        t.on_kill(move || f.store(true, Ordering::SeqCst));

        assert!(t.kill(), "a running task reported nothing to kill");
        assert!(fired.load(Ordering::SeqCst), "the killer never ran");
        assert_eq!(t.state(), TaskState::Killed);

        // Twice is not an error, and it is not a second kill either. Reporting
        // "stopped it" for a task that was already dead is exactly the plausible
        // success this project treats as a defect.
        assert!(!t.kill(), "killing a dead task claimed to have stopped it");
    }

    /// A task that exited on its own is not killable, and its status survives.
    ///
    /// **The registered killer outlives the child.** `kill` used to fire it for
    /// any task that still had one — so a task that had already exited was
    /// signalled anyway (a pid that is gone, or one the operating system has
    /// since handed to somebody else) and its real exit status was overwritten
    /// with `Killed`. The caller was told it had stopped something it had not.
    ///
    /// Found by the agent building `KillShell`, which guarded it by checking
    /// `state()` before calling `kill()`. That cannot be complete at the caller:
    /// the check and the call are two separate locks, and the task can exit
    /// between them. The check belongs here, under the lock that owns the state.
    #[test]
    fn a_task_that_exited_on_its_own_cannot_be_killed_and_keeps_its_status() {
        use std::sync::atomic::{AtomicBool, Ordering};
        let r = Registry::new();
        let t = r.spawn("s1", "cargo build");
        let fired = Arc::new(AtomicBool::new(false));
        let f = fired.clone();
        t.on_kill(move || f.store(true, Ordering::SeqCst));

        // It finishes on its own, with a status somebody will want.
        t.set_state(TaskState::Exited(Some(0)));

        assert!(!t.kill(), "a task that had already exited reported a kill");
        assert!(
            !fired.load(Ordering::SeqCst),
            "a stale killer was fired at a process that had already gone"
        );
        assert_eq!(
            t.state(),
            TaskState::Exited(Some(0)),
            "the real exit status was overwritten with Killed"
        );
    }

    #[test]
    fn kill_all_returns_only_what_it_actually_signalled() {
        let r = Registry::new();
        let running = r.spawn("s1", "sleep 100");
        running.on_kill(|| {});
        let done = r.spawn("s1", "echo hi");
        done.set_state(TaskState::Exited(Some(0)));
        let other = r.spawn("s2", "not mine");
        other.on_kill(|| {});

        let killed = r.kill_all("s1");
        assert_eq!(
            killed.len(),
            1,
            "the exit path over-reported what it stopped"
        );
        assert_eq!(killed[0].id, running.id);
        assert_eq!(other.state(), TaskState::Running, "another session was hit");
    }
}
