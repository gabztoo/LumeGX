//! Diagnostics: draws and dispatches that read what only dropped work writes.
//!
//! A draw or dispatch the host cannot run (its shader does not translate, its
//! pipeline is refused or skipped) leaves what it would have written as the
//! title last saw it. A later pass that reads that, or waits for a value in
//! it, works on stale data or never finishes, which on some hosts is a
//! device hang far from the dropped work. This module keeps what dropped work
//! would have written (its unordered-access views and render targets), forgets
//! a range again when work that runs writes it, and logs once per pair
//! a reader that runs reading one of them, with the shader that was dropped.

use std::collections::{HashSet, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use crate::descriptor::View;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Target {
    /// Title memory, `start..end`.
    Buffer(u64, u64),
    /// A title texture object.
    Texture(usize),
}

impl Target {
    fn overlaps(self, other: Target) -> bool {
        match (self, other) {
            (Target::Buffer(a, b), Target::Buffer(c, d)) => a < d && c < b,
            (Target::Texture(a), Target::Texture(b)) => a == b,
            _ => false,
        }
    }

    fn describe(self) -> String {
        match self {
            Target::Buffer(a, b) => format!("buffer {a:#x}+{:#x}", b - a),
            Target::Texture(t) => {
                let desc = crate::resource::host_resource(t as crate::object::This).map(|r| {
                    // SAFETY: a live resource.
                    let d = unsafe { r.GetDesc() };
                    format!(" {}x{} f{}", d.Width, d.Height, d.Format.0)
                });
                format!("texture {t:#x}{}", desc.unwrap_or_default())
            }
        }
    }
}

struct Write {
    what: Target,
    /// Shared: a read that finds it clones it.
    writer: Arc<str>,
    frame: u64,
}

const MAX_WRITES: usize = 8192;

static WRITES: Mutex<VecDeque<Write>> = Mutex::new(VecDeque::new());
/// Whether [`WRITES`] holds anything (stored with it locked): a read checks
/// this first, so the common case, nothing dropped, takes no lock. It is
/// read relaxed and before the lock, so a read racing the very first drop
/// of a frame on another thread can miss it: one log line late, which a
/// detector can afford.
static PENDING: AtomicBool = AtomicBool::new(false);
/// A (reader, dropped writer) pair already logged.
type Pair = (String, Arc<str>);
static SEEN: Mutex<Option<HashSet<Pair>>> = Mutex::new(None);

thread_local! {
    /// The pipeline the draw or dispatch being bound on this thread runs.
    static READER: std::cell::RefCell<String> = const { std::cell::RefCell::new(String::new()) };
}

/// Whether the detector runs (`VICTORY_PRODUCERS=1`).
pub(crate) fn on() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("VICTORY_PRODUCERS").is_some_and(|v| v == "1"))
}

/// The pipeline a later [`read`] or [`live_write`] on this thread belongs to.
pub(crate) fn set_reader(name: &str) {
    if on() {
        READER.with(|r| {
            let mut r = r.borrow_mut();
            r.clear();
            r.push_str(name);
        });
    }
}

fn target_of(view: &View) -> Option<Target> {
    let placed = crate::descriptor::host::placed_texture(view);
    let view = placed.as_ref().unwrap_or(view);
    if let Some((address, len)) = crate::descriptor::host::buffer_range(view)
        && len > 0
    {
        return Some(Target::Buffer(address, address + len));
    }
    match *view {
        View::Shader(r, _) | View::Unordered(r, _, _) if r != 0 => Some(Target::Texture(r)),
        _ => None,
    }
}

/// Forgets what dropped work wrote before the frame before `frame` (the
/// last presented): that is history, the title has moved on.
fn prune(writes: &mut VecDeque<Write>, frame: u64) {
    writes.retain(|w| w.frame + 2 > frame);
}

/// Notes whether `writes` (the locked [`WRITES`]) holds anything, for
/// [`read`]'s check before the lock.
fn settle(writes: &VecDeque<Write>) {
    PENDING.store(!writes.is_empty(), Ordering::Relaxed);
}

/// Notes what dropped work `writer` would have written.
fn record(what: Target, writer: &str) {
    let frame = crate::queue::presented();
    let mut writes = WRITES.lock().unwrap_or_else(|e| e.into_inner());
    prune(&mut writes, frame);
    push(&mut writes, what, writer, frame);
    settle(&writes);
}

/// Adds a write of `what` by `writer` at `frame` to `writes`. A write
/// already there is moved to the back with the new frame (so a writer
/// dropped every frame never ages out); only a new one evicts the oldest
/// when the queue is full, so a duplicate never costs an unrelated entry.
fn push(writes: &mut VecDeque<Write>, what: Target, writer: &str, frame: u64) {
    if let Some(i) = writes.iter().position(|w| w.what == what && &*w.writer == writer)
        && let Some(mut w) = writes.remove(i)
    {
        w.frame = frame;
        writes.push_back(w);
        return;
    }
    if writes.len() >= MAX_WRITES {
        writes.pop_front();
    }
    writes.push_back(Write {
        what,
        writer: writer.into(),
        frame,
    });
}

/// Dropped work `writer` would have written through `view`.
pub(crate) fn dropped_write(writer: &str, view: &View) {
    if on()
        && let Some(what) = target_of(view)
    {
        record(what, writer);
    }
}

/// Dropped draw `writer` would have rendered to title texture `resource`.
pub(crate) fn dropped_target(writer: &str, resource: usize) {
    if on() && resource != 0 {
        record(Target::Texture(resource), writer);
    }
}

/// Work that runs writes through `view`: what it writes is produced.
pub(crate) fn live_write(view: &View) {
    if !on() {
        return;
    }
    if PENDING.load(Ordering::Relaxed)
        && let Some(what) = target_of(view)
    {
        let mut writes = WRITES.lock().unwrap_or_else(|e| e.into_inner());
        clear_written(&mut writes, what);
        settle(&writes);
    }
}

fn clear_written(writes: &mut VecDeque<Write>, what: Target) {
    // Process only the original entries so each fragment keeps its place in the history.
    for _ in 0..writes.len() {
        let mut write = writes.pop_front().unwrap();
        if !write.what.overlaps(what) {
            writes.push_back(write);
            continue;
        }
        let (Target::Buffer(start, end), Target::Buffer(live_start, live_end)) = (write.what, what)
        else {
            continue;
        };

        let left = (start < live_start).then_some(Target::Buffer(start, live_start));
        let right = (live_end < end).then_some(Target::Buffer(live_end, end));
        match (left, right) {
            (Some(what), Some(tail)) => {
                let tail = Write {
                    what: tail,
                    writer: write.writer.clone(),
                    frame: write.frame,
                };
                write.what = what;
                writes.push_back(write);
                writes.push_back(tail);
            }
            (Some(what), None) | (None, Some(what)) => {
                write.what = what;
                writes.push_back(write);
            }
            (None, None) => {}
        }
    }

    let excess = writes.len().saturating_sub(MAX_WRITES);
    writes.drain(..excess);
}

/// A draw that runs renders to title texture `resource`.
pub(crate) fn live_target(resource: usize) {
    if on() && resource != 0 && PENDING.load(Ordering::Relaxed) {
        let mut writes = WRITES.lock().unwrap_or_else(|e| e.into_inner());
        writes.retain(|w| w.what != Target::Texture(resource));
        settle(&writes);
    }
}

/// Frame `frame` was presented: forgets history (see [`prune`]) once a
/// frame, so [`read`], which runs per bound resource, never walks it.
pub(crate) fn presented(frame: u64) {
    if on() && PENDING.load(Ordering::Relaxed) {
        let mut writes = WRITES.lock().unwrap_or_else(|e| e.into_inner());
        prune(&mut writes, frame);
        settle(&writes);
    }
}

/// Work that runs reads through `view`: logs it once per (reader, dropped
/// writer) if only dropped work wrote that. History is forgotten at each
/// present ([`presented`]), so it is never logged against a reader.
pub(crate) fn read(view: &View) {
    if !on() || !PENDING.load(Ordering::Relaxed) {
        return;
    }
    let Some(what) = target_of(view) else { return };
    let hits: Vec<(Arc<str>, Target)> = {
        let writes = WRITES.lock().unwrap_or_else(|e| e.into_inner());
        writes
            .iter()
            .filter(|w| w.what.overlaps(what))
            .map(|w| (w.writer.clone(), w.what))
            .collect()
    };
    if hits.is_empty() {
        return;
    }
    let reader = READER.with(|r| r.borrow().clone());
    let mut seen = SEEN.lock().unwrap_or_else(|e| e.into_inner());
    let seen = seen.get_or_insert_with(HashSet::new);
    for (writer, written) in hits {
        if seen.insert((reader.clone(), writer.clone())) {
            tracing::warn!(
                reader = %reader,
                dropped = %writer,
                what = %written.describe(),
                frame = crate::queue::presented(),
                "reads what only dropped work writes"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frames(writes: &VecDeque<Write>) -> Vec<(Target, u64)> {
        writes.iter().map(|w| (w.what, w.frame)).collect()
    }

    /// A writer dropped again refreshes its frame instead of aging out.
    #[test]
    fn a_repeated_drop_refreshes_its_frame() {
        let mut writes = VecDeque::new();
        push(&mut writes, Target::Texture(1), "pixel a", 9);
        push(&mut writes, Target::Texture(1), "pixel a", 10);
        assert_eq!(frames(&writes), vec![(Target::Texture(1), 10)]);
        prune(&mut writes, 11);
        assert_eq!(writes.len(), 1, "still there for a read in frame 12");
    }

    /// At the cap, a duplicate that is not the oldest evicts nothing.
    #[test]
    fn a_duplicate_at_the_cap_evicts_nothing() {
        let mut writes = VecDeque::new();
        for i in 0..MAX_WRITES {
            push(&mut writes, Target::Texture(i), "compute c", 5);
        }
        push(&mut writes, Target::Texture(7), "compute c", 6);
        assert_eq!(writes.len(), MAX_WRITES);
        assert!(writes.iter().any(|w| w.what == Target::Texture(0)), "the oldest is kept");
        assert_eq!(writes.back().map(|w| (w.what, w.frame)), Some((Target::Texture(7), 6)));
        // A new one at the cap evicts the oldest.
        push(&mut writes, Target::Texture(MAX_WRITES), "compute c", 6);
        assert_eq!(writes.len(), MAX_WRITES);
        assert!(!writes.iter().any(|w| w.what == Target::Texture(0)));
    }

    /// A null view (resource 0) names no target, so two of them never
    /// pair a reader with a dropped writer.
    #[test]
    fn a_null_view_is_no_target() {
        assert_eq!(target_of(&View::Shader(0, None)), None);
        assert_eq!(target_of(&View::Unordered(0, 0, None)), None);
    }

    fn missing(what: Target) -> Write {
        Write {
            what,
            writer: "synthetic dispatch".into(),
            frame: 9,
        }
    }

    #[test]
    fn a_live_buffer_write_only_clears_its_own_range() {
        let readers = [(8, 10), (30, 32), (48, 50)];
        for (live_start, live_end, expected) in [
            (0, 8, [true, true, true]),
            (0, 24, [false, true, true]),
            (24, 40, [true, false, true]),
            (40, 80, [true, true, false]),
            (8, 72, [false, false, false]),
        ] {
            let mut writes =
                VecDeque::from([missing(Target::Buffer(8, 72)), missing(Target::Texture(1))]);
            clear_written(&mut writes, Target::Buffer(live_start, live_end));
            let actual = readers.map(|(start, end)| {
                writes
                    .iter()
                    .any(|write| write.what.overlaps(Target::Buffer(start, end)))
            });
            assert_eq!(actual, expected, "live write {live_start}..{live_end}");
            assert!(writes.iter().any(|write| write.what == Target::Texture(1)));
            assert!(
                writes
                    .iter()
                    .all(|write| &*write.writer == "synthetic dispatch" && write.frame == 9)
            );
        }
    }

    #[test]
    fn writes_before_the_frame_before_last_are_forgotten() {
        let mut writes: VecDeque<_> = (7..=10)
            .map(|frame| Write {
                frame,
                ..missing(Target::Texture(frame as usize))
            })
            .collect();
        prune(&mut writes, 10);
        let frames: Vec<u64> = writes.iter().map(|w| w.frame).collect();
        assert_eq!(frames, [9, 10]);
        prune(&mut writes, 13);
        assert!(writes.is_empty());
    }

    #[test]
    fn splitting_ranges_keeps_the_newest_frames_when_full() {
        let mut writes: VecDeque<_> = (0..MAX_WRITES)
            .map(|frame| Write {
                frame: frame as u64,
                ..missing(Target::Buffer(8, 72))
            })
            .collect();

        clear_written(&mut writes, Target::Buffer(24, 40));

        assert_eq!(writes.len(), MAX_WRITES);
        assert_eq!(writes.front().unwrap().frame, (MAX_WRITES / 2) as u64);
        assert!(writes[MAX_WRITES - 2].what == Target::Buffer(8, 24));
        assert!(writes.back().unwrap().what == Target::Buffer(40, 72));
        assert!(
            writes
                .iter()
                .all(|write| write.frame >= (MAX_WRITES / 2) as u64)
        );
    }
}
