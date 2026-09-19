//! Kernel objects, threads and the scheduler (3dbrew, "SVC", "Multi-threading"
//! and "Kernel object types").
//!
//! Threads have a priority from 0 (most urgent) to 63. The most urgent thread
//! that can run does, until it blocks, yields or a more urgent one becomes
//! ready; threads of one priority take turns only when one of them gives way.
//! All threads share one processor for now: the second application core is a
//! later step, and until then a thread's processor number is recorded and
//! otherwise ignored.
//!
//! A thread waits on objects. An object is *available* to a thread when
//! waiting on it would not block: a signalled event or timer, a mutex nobody
//! else holds, a semaphore with a count, a thread that has ended. Taking it
//! may change it: a one-shot event clears, a mutex gets its owner, a
//! semaphore counts down. When an object becomes available, the waiting
//! threads are looked at in order of priority, then of arrival.

use crate::result::{self, ResultCode};
use arm_core::Context;

pub type ThreadId = usize;
pub type ObjId = usize;
pub type Handle = u32;

/// The pseudo-handles a thread names itself and its process with.
pub const CURRENT_THREAD: Handle = 0xFFFF_8000;
pub const CURRENT_PROCESS: Handle = 0xFFFF_8001;

pub const LOWEST_PRIORITY: u8 = 63;

/// How an event or timer behaves once signalled.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Reset {
    /// Wakes one waiter and clears.
    OneShot,
    /// Stays signalled until cleared by hand.
    Sticky,
    /// Wakes everyone waiting and clears.
    Pulse,
}

impl Reset {
    pub fn from_raw(raw: u32) -> Self {
        match raw {
            0 => Reset::OneShot,
            1 => Reset::Sticky,
            _ => Reset::Pulse,
        }
    }
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Object {
    Thread(ThreadId),
    Process,
    Event {
        signalled: bool,
        reset: Reset,
    },
    Mutex {
        owner: Option<ThreadId>,
        depth: u32,
    },
    Semaphore {
        count: i32,
        max: i32,
    },
    Timer {
        signalled: bool,
        reset: Reset,
        /// Cycles between firings after the first; zero for a single shot.
        interval: u64,
    },
    Arbiter,
    /// Physical memory a process can map: `pa`, `len`.
    SharedMemory {
        pa: u32,
        len: u32,
    },
    /// The client end of a session with the service of that index.
    Session {
        service: usize,
    },
}

/// What a thread that is not running or ready is waiting for.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Wait {
    /// Time to pass.
    Sleep,
    /// One of, or all of, some objects.
    Objects { ids: Vec<ObjId>, all: bool },
    /// A signal on an address of an arbiter.
    Arbiter { address: u32 },
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum State {
    Ready,
    Running,
    Waiting(Wait),
    Dead,
}

pub struct Thread {
    pub context: Context,
    pub priority: u8,
    pub processor: i32,
    pub tls: u32,
    pub state: State,
    /// Position among the ready threads of its priority: lower runs first.
    turn: u64,
    /// Mutexes held, to release when the thread ends.
    held: Vec<ObjId>,
}

/// A wait ended by its timeout: description 1022 of module OS, the code
/// libctru tests for.
pub const TIMEOUT: ResultCode = ResultCode(0x0940_1BFE);
pub const INVALID_HANDLE: ResultCode = ResultCode(0xD8E0_07F7);
/// Releasing a mutex another thread holds.
pub const NOT_OWNER: ResultCode = ResultCode(0xD8E0_041F);
/// A semaphore count past its maximum.
pub const OUT_OF_RANGE: ResultCode = ResultCode(0xE0E0_1BFD);

#[derive(Default)]
pub struct Kernel {
    pub threads: Vec<Thread>,
    pub objects: Vec<Object>,
    handles: Vec<Option<ObjId>>,
    /// The thread whose registers are in the processor.
    pub current: Option<ThreadId>,
    turns: u64,
    /// Threads whose timeout should be cancelled or armed by the caller, who
    /// owns the clock: `(thread, Some(nanoseconds))` arms, `None` cancels.
    pub timeouts: Vec<(ThreadId, Option<u64>)>,
}

impl Kernel {
    pub fn new() -> Self {
        Kernel::default()
    }

    // ---- objects and handles -------------------------------------------

    pub fn create(&mut self, object: Object) -> ObjId {
        self.objects.push(object);
        self.objects.len() - 1
    }

    /// A new handle for `object`. Handles are small numbers, never zero.
    pub fn open(&mut self, object: ObjId) -> Handle {
        let slot = match self.handles.iter().position(Option::is_none) {
            Some(slot) => slot,
            None => {
                self.handles.push(None);
                self.handles.len() - 1
            }
        };
        self.handles[slot] = Some(object);
        slot as Handle + 1
    }

    pub fn create_handle(&mut self, object: Object) -> Handle {
        let id = self.create(object);
        self.open(id)
    }

    pub fn close(&mut self, handle: Handle) -> ResultCode {
        match (handle as usize)
            .checked_sub(1)
            .and_then(|slot| self.handles.get_mut(slot))
        {
            Some(slot) if slot.is_some() => {
                *slot = None;
                result::SUCCESS
            }
            _ if handle == CURRENT_THREAD || handle == CURRENT_PROCESS => result::SUCCESS,
            _ => INVALID_HANDLE,
        }
    }

    /// The object behind a handle, the pseudo-handles included.
    pub fn resolve(&self, handle: Handle) -> Option<ObjId> {
        if handle == CURRENT_THREAD {
            let current = self.current?;
            return self
                .objects
                .iter()
                .position(|o| *o == Object::Thread(current));
        }
        if handle == CURRENT_PROCESS {
            return self.objects.iter().position(|o| *o == Object::Process);
        }
        *self.handles.get((handle as usize).checked_sub(1)?)?
    }

    pub fn object(&self, handle: Handle) -> Option<&Object> {
        self.resolve(handle).map(|id| &self.objects[id])
    }

    // ---- threads --------------------------------------------------------

    /// A new thread, ready to run. Returns its id and the id of its object.
    pub fn spawn(
        &mut self,
        context: Context,
        priority: u8,
        processor: i32,
        tls: u32,
    ) -> (ThreadId, ObjId) {
        self.turns += 1;
        self.threads.push(Thread {
            context,
            priority: priority.min(LOWEST_PRIORITY),
            processor,
            tls,
            state: State::Ready,
            turn: self.turns,
            held: Vec::new(),
        });
        let id = self.threads.len() - 1;
        (id, self.create(Object::Thread(id)))
    }

    fn make_ready(&mut self, thread: ThreadId) {
        self.turns += 1;
        let t = &mut self.threads[thread];
        t.state = State::Ready;
        t.turn = self.turns;
        self.timeouts.push((thread, None));
    }

    /// The thread that should have the processor: the running one, unless a
    /// more urgent thread is ready or it cannot go on.
    pub fn pick(&self) -> Option<ThreadId> {
        let best_ready = self
            .threads
            .iter()
            .enumerate()
            .filter(|(_, t)| t.state == State::Ready)
            .min_by_key(|(_, t)| (t.priority, t.turn))
            .map(|(id, _)| id);
        match (self.current, best_ready) {
            (Some(current), Some(ready)) if self.threads[current].state == State::Running => {
                if self.threads[ready].priority < self.threads[current].priority {
                    Some(ready)
                } else {
                    Some(current)
                }
            }
            (Some(current), None) if self.threads[current].state == State::Running => Some(current),
            (_, ready) => ready,
        }
    }

    /// Give the processor to `next` (or to nobody). The caller has saved the
    /// outgoing thread's registers and loads the incoming one's.
    pub fn switch_to(&mut self, next: Option<ThreadId>) {
        if let Some(current) = self.current {
            let t = &mut self.threads[current];
            if t.state == State::Running {
                // Pre-empted: it keeps its place in its priority.
                t.state = State::Ready;
            }
        }
        if let Some(next) = next {
            self.threads[next].state = State::Running;
        }
        self.current = next;
    }

    /// The running thread gives way to the others of its priority.
    pub fn yield_now(&mut self) {
        if let Some(current) = self.current {
            self.make_ready(current);
        }
    }

    pub fn sleep(&mut self, nanoseconds: u64) {
        if let Some(current) = self.current {
            self.threads[current].state = State::Waiting(Wait::Sleep);
            self.timeouts.push((current, Some(nanoseconds)));
        }
    }

    /// End the running thread: its mutexes are released and whoever waits
    /// for it wakes.
    pub fn exit_current(&mut self) {
        let Some(current) = self.current else {
            return;
        };
        self.threads[current].state = State::Dead;
        for mutex in std::mem::take(&mut self.threads[current].held) {
            self.objects[mutex] = Object::Mutex {
                owner: None,
                depth: 0,
            };
            self.became_available(mutex);
        }
        if let Some(object) = self
            .objects
            .iter()
            .position(|o| *o == Object::Thread(current))
        {
            self.became_available(object);
        }
    }

    pub fn all_dead(&self) -> bool {
        self.threads.iter().all(|t| t.state == State::Dead)
    }

    // ---- waiting --------------------------------------------------------

    fn available(&self, object: ObjId, thread: ThreadId) -> bool {
        match &self.objects[object] {
            Object::Thread(id) => self.threads[*id].state == State::Dead,
            Object::Event { signalled, .. } | Object::Timer { signalled, .. } => *signalled,
            Object::Mutex { owner, .. } => owner.is_none_or(|o| o == thread),
            Object::Semaphore { count, .. } => *count > 0,
            // Waiting on anything else never blocks.
            _ => true,
        }
    }

    fn take(&mut self, object: ObjId, thread: ThreadId) {
        match &mut self.objects[object] {
            Object::Event { signalled, reset }
            | Object::Timer {
                signalled, reset, ..
            } => {
                if *reset != Reset::Sticky {
                    *signalled = false;
                }
            }
            Object::Mutex { owner, depth } => {
                if owner.is_none() {
                    self.threads[thread].held.push(object);
                }
                *owner = Some(thread);
                *depth += 1;
            }
            Object::Semaphore { count, .. } => *count -= 1,
            _ => {}
        }
    }

    /// Wait for one of `ids`, or for all of them. `Ok(index)` if the wait is
    /// over at once; otherwise the running thread blocks (and `timeout`, if
    /// not negative, is armed) and the results reach its registers later.
    pub fn wait(&mut self, ids: Vec<ObjId>, all: bool, timeout: i64) -> Option<(ResultCode, u32)> {
        let current = self.current?;
        if let Some(index) = self.try_take(&ids, all, current) {
            return Some((result::SUCCESS, index));
        }
        if timeout == 0 {
            return Some((TIMEOUT, 0));
        }
        self.threads[current].state = State::Waiting(Wait::Objects { ids, all });
        if timeout > 0 {
            self.timeouts.push((current, Some(timeout as u64)));
        }
        None
    }

    fn try_take(&mut self, ids: &[ObjId], all: bool, thread: ThreadId) -> Option<u32> {
        if all {
            if !ids.iter().all(|&id| self.available(id, thread)) {
                return None;
            }
            for &id in ids {
                self.take(id, thread);
            }
            // The index is not meaningful for a wait on everything.
            return Some(0);
        }
        let index = ids.iter().position(|&id| self.available(id, thread))?;
        self.take(ids[index], thread);
        Some(index as u32)
    }

    /// `object` may have become available: wake whom it satisfies, the most
    /// urgent and longest-waiting first.
    pub fn became_available(&mut self, object: ObjId) {
        let mut waiters: Vec<ThreadId> = (0..self.threads.len())
            .filter(|&id| {
                matches!(&self.threads[id].state,
                    State::Waiting(Wait::Objects { ids, .. }) if ids.contains(&object))
            })
            .collect();
        waiters.sort_by_key(|&id| (self.threads[id].priority, self.threads[id].turn));
        let pulse = matches!(
            self.objects[object],
            Object::Event {
                reset: Reset::Pulse,
                ..
            } | Object::Timer {
                reset: Reset::Pulse,
                ..
            }
        );
        for thread in waiters {
            let State::Waiting(Wait::Objects { ids, all }) = self.threads[thread].state.clone()
            else {
                continue;
            };
            // A pulse reaches every waiter before it clears.
            if pulse && !all {
                let index = ids.iter().position(|&id| id == object).unwrap_or(0);
                self.finish_wait(thread, result::SUCCESS, index as u32);
                continue;
            }
            if let Some(index) = self.try_take(&ids, all, thread) {
                self.finish_wait(thread, result::SUCCESS, index);
            }
        }
        if pulse {
            if let Object::Event { signalled, .. } | Object::Timer { signalled, .. } =
                &mut self.objects[object]
            {
                *signalled = false;
            }
        }
    }

    fn finish_wait(&mut self, thread: ThreadId, code: ResultCode, index: u32) {
        let context = &mut self.threads[thread].context;
        context.r[0] = code.0;
        context.r[1] = index;
        self.make_ready(thread);
    }

    /// A thread's timeout ran out.
    pub fn timed_out(&mut self, thread: ThreadId) {
        match self.threads[thread].state {
            State::Waiting(Wait::Sleep) => self.make_ready(thread),
            State::Waiting(_) => self.finish_wait(thread, TIMEOUT, 0),
            _ => {}
        }
    }

    // ---- the objects' own calls -----------------------------------------

    pub fn signal(&mut self, object: ObjId) {
        if let Object::Event { signalled, .. } | Object::Timer { signalled, .. } =
            &mut self.objects[object]
        {
            *signalled = true;
            self.became_available(object);
        }
    }

    pub fn clear(&mut self, object: ObjId) {
        if let Object::Event { signalled, .. } | Object::Timer { signalled, .. } =
            &mut self.objects[object]
        {
            *signalled = false;
        }
    }

    pub fn release_mutex(&mut self, object: ObjId) -> ResultCode {
        let Some(current) = self.current else {
            return INVALID_HANDLE;
        };
        match &mut self.objects[object] {
            Object::Mutex { owner, depth } if *owner == Some(current) => {
                *depth -= 1;
                if *depth == 0 {
                    *owner = None;
                    self.threads[current].held.retain(|&m| m != object);
                    self.became_available(object);
                }
                result::SUCCESS
            }
            Object::Mutex { .. } => NOT_OWNER,
            _ => INVALID_HANDLE,
        }
    }

    /// Returns the count before the release.
    pub fn release_semaphore(&mut self, object: ObjId, by: i32) -> Result<i32, ResultCode> {
        match &mut self.objects[object] {
            Object::Semaphore { count, max } => {
                if by < 0 || count.checked_add(by).is_none_or(|sum| sum > *max) {
                    return Err(OUT_OF_RANGE);
                }
                let before = *count;
                *count += by;
                self.became_available(object);
                Ok(before)
            }
            _ => Err(INVALID_HANDLE),
        }
    }

    /// Wake up to `count` threads (all of them if negative) waiting on
    /// `address`, the most urgent first.
    pub fn arbiter_signal(&mut self, address: u32, count: i32) {
        let mut waiters: Vec<ThreadId> = (0..self.threads.len())
            .filter(|&id| self.threads[id].state == State::Waiting(Wait::Arbiter { address }))
            .collect();
        waiters.sort_by_key(|&id| (self.threads[id].priority, self.threads[id].turn));
        let count = if count < 0 {
            waiters.len()
        } else {
            count as usize
        };
        for thread in waiters.into_iter().take(count) {
            self.finish_wait(thread, result::SUCCESS, 0);
        }
    }

    /// Block the running thread on `address`, with a timeout if not negative.
    pub fn arbiter_wait(&mut self, address: u32, timeout: i64) {
        if let Some(current) = self.current {
            self.threads[current].state = State::Waiting(Wait::Arbiter { address });
            if timeout >= 0 {
                self.timeouts.push((current, Some(timeout as u64)));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use arm_core::{Arch, Cpu};

    fn context() -> Context {
        Cpu::new_user(Arch::V6k, 0x10_0000, 0x1000_0000).save_context()
    }

    /// A kernel with threads of the given priorities, the first one running.
    fn kernel(priorities: &[u8]) -> Kernel {
        let mut k = Kernel::new();
        for &priority in priorities {
            k.spawn(context(), priority, 0, 0);
        }
        let first = k.pick();
        k.switch_to(first);
        k
    }

    fn run_next(k: &mut Kernel) -> Option<ThreadId> {
        let next = k.pick();
        if next != k.current {
            k.switch_to(next);
        }
        k.current
    }

    #[test]
    fn the_most_urgent_ready_thread_runs_and_equals_take_turns_only_on_yield() {
        let mut k = kernel(&[0x30, 0x30, 0x20]);
        assert_eq!(k.current, Some(2), "priority 0x20 beats 0x30");
        assert_eq!(run_next(&mut k), Some(2), "nothing pre-empts it");
        k.sleep(1000);
        assert_eq!(run_next(&mut k), Some(0), "the older of the two equals");
        assert_eq!(run_next(&mut k), Some(0), "an equal does not pre-empt");
        k.yield_now();
        assert_eq!(run_next(&mut k), Some(1));
        k.timed_out(2);
        assert_eq!(run_next(&mut k), Some(2), "the sleeper pre-empts on waking");
        assert_eq!(k.threads[1].state, State::Ready);
    }

    #[test]
    fn handles_are_reused_and_pseudo_handles_name_the_caller() {
        let mut k = kernel(&[0x30]);
        let event = k.create_handle(Object::Event {
            signalled: false,
            reset: Reset::OneShot,
        });
        assert_eq!(event, 1);
        assert_eq!(k.close(event), result::SUCCESS);
        assert_eq!(k.close(event), INVALID_HANDLE);
        assert_eq!(k.object(event), None);
        assert_eq!(k.create_handle(Object::Arbiter), 1, "the slot is reused");
        assert_eq!(k.object(CURRENT_THREAD), Some(&Object::Thread(0)));
        assert_eq!(k.resolve(0), None);
    }

    #[test]
    fn a_one_shot_event_wakes_one_waiter_a_sticky_one_all() {
        let mut k = kernel(&[0x30, 0x30, 0x30]);
        let one_shot = k.create(Object::Event {
            signalled: false,
            reset: Reset::OneShot,
        });
        let sticky = k.create(Object::Event {
            signalled: false,
            reset: Reset::Sticky,
        });
        for (thread, object) in [(0, one_shot), (1, one_shot), (2, sticky)] {
            assert_eq!(run_next(&mut k), Some(thread));
            assert_eq!(k.wait(vec![object], false, -1), None);
        }
        assert_eq!(run_next(&mut k), None, "everyone waits");

        k.signal(one_shot);
        assert_eq!(k.threads[0].state, State::Ready);
        assert!(matches!(k.threads[1].state, State::Waiting(_)));
        assert_eq!(k.threads[0].context.r[0], 0);
        k.signal(sticky);
        assert_eq!(k.threads[2].state, State::Ready);
        // Still signalled: a later wait does not block.
        assert_eq!(run_next(&mut k), Some(0));
        assert_eq!(k.wait(vec![sticky], false, -1), Some((result::SUCCESS, 0)));
        assert_eq!(k.wait(vec![one_shot], false, 0), Some((TIMEOUT, 0)));
    }

    #[test]
    fn a_wait_names_the_object_that_ended_it_or_times_out() {
        let mut k = kernel(&[0x30, 0x31]);
        let a = k.create(Object::Event {
            signalled: false,
            reset: Reset::OneShot,
        });
        let b = k.create(Object::Semaphore { count: 0, max: 2 });
        assert_eq!(k.wait(vec![a, b], false, 5_000_000), None);
        assert_eq!(k.timeouts.pop(), Some((0, Some(5_000_000))));
        assert_eq!(k.release_semaphore(b, 1), Ok(0));
        assert_eq!(
            k.threads[0].context.r[..2],
            [0, 1],
            "index 1 ended the wait"
        );
        assert_eq!(k.objects[b], Object::Semaphore { count: 0, max: 2 });
        assert_eq!(
            k.timeouts.pop(),
            Some((0, None)),
            "the timeout is cancelled"
        );
        assert_eq!(k.release_semaphore(b, 3), Err(OUT_OF_RANGE));

        assert_eq!(run_next(&mut k), Some(0));
        assert_eq!(k.wait(vec![a, b], true, 1000), None);
        k.signal(a);
        assert!(
            matches!(k.threads[0].state, State::Waiting(_)),
            "it wants both"
        );
        k.timed_out(0);
        assert_eq!(k.threads[0].context.r[0], TIMEOUT.0);
    }

    #[test]
    fn a_mutex_has_an_owner_a_depth_and_passes_to_the_next_in_line() {
        let mut k = kernel(&[0x30, 0x30]);
        let mutex = k.create(Object::Mutex {
            owner: None,
            depth: 0,
        });
        assert_eq!(k.wait(vec![mutex], false, -1), Some((result::SUCCESS, 0)));
        assert_eq!(k.wait(vec![mutex], false, -1), Some((result::SUCCESS, 0)));
        k.yield_now();
        assert_eq!(run_next(&mut k), Some(1));
        assert_eq!(k.release_mutex(mutex), NOT_OWNER);
        assert_eq!(k.wait(vec![mutex], false, -1), None);

        assert_eq!(run_next(&mut k), Some(0));
        assert_eq!(k.release_mutex(mutex), result::SUCCESS);
        assert!(
            matches!(k.threads[1].state, State::Waiting(_)),
            "held twice"
        );
        assert_eq!(k.release_mutex(mutex), result::SUCCESS);
        assert_eq!(k.threads[1].state, State::Ready);
        assert_eq!(
            k.objects[mutex],
            Object::Mutex {
                owner: Some(1),
                depth: 1
            }
        );
    }

    #[test]
    fn a_thread_that_ends_frees_its_mutexes_and_wakes_its_joiners() {
        let mut k = kernel(&[0x30, 0x31]);
        let mutex = k.create(Object::Mutex {
            owner: None,
            depth: 0,
        });
        k.wait(vec![mutex], false, -1);
        k.sleep(1);
        assert_eq!(run_next(&mut k), Some(1));
        let thread0 = k.resolve(CURRENT_THREAD).unwrap() - 1;
        assert_eq!(k.objects[thread0], Object::Thread(0));
        assert_eq!(k.wait(vec![thread0, mutex], true, -1), None);

        k.timed_out(0);
        assert_eq!(run_next(&mut k), Some(0));
        k.exit_current();
        assert_eq!(
            k.threads[1].state,
            State::Ready,
            "joined, and it has the mutex"
        );
        assert_eq!(
            k.objects[mutex],
            Object::Mutex {
                owner: Some(1),
                depth: 1
            }
        );
        assert!(!k.all_dead());
    }

    #[test]
    fn the_arbiter_wakes_the_most_urgent_waiters_first() {
        let mut k = kernel(&[0x30, 0x20, 0x28]);
        for thread in [1, 2, 0] {
            assert_eq!(run_next(&mut k), Some(thread));
            k.arbiter_wait(0x0800_0000, -1);
        }
        k.arbiter_signal(0x0800_0004, -1);
        assert_eq!(run_next(&mut k), None, "another address");
        k.arbiter_signal(0x0800_0000, 2);
        assert_eq!(k.threads[1].state, State::Ready);
        assert_eq!(k.threads[2].state, State::Ready);
        assert!(matches!(k.threads[0].state, State::Waiting(_)));
        k.arbiter_signal(0x0800_0000, -1);
        assert_eq!(k.threads[0].state, State::Ready);
    }

    #[test]
    fn a_pulse_reaches_every_waiter_and_leaves_the_event_clear() {
        let mut k = kernel(&[0x30, 0x30]);
        let pulse = k.create(Object::Event {
            signalled: false,
            reset: Reset::Pulse,
        });
        for thread in [0, 1] {
            assert_eq!(run_next(&mut k), Some(thread));
            k.wait(vec![pulse], false, -1);
        }
        k.signal(pulse);
        assert_eq!(k.threads[0].state, State::Ready);
        assert_eq!(k.threads[1].state, State::Ready);
        assert_eq!(
            k.objects[pulse],
            Object::Event {
                signalled: false,
                reset: Reset::Pulse
            }
        );
    }
}
