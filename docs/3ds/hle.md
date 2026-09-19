# 3DS high-level mode

What `crates/ctr-hle` does in place of Nintendo's operating system, with the
source of each fact. The processor is `arm-core`'s ARMv6K with VFPv2 in user
mode; memory is `ctr_core::bus::PhysMem`; the GPU block is
`ctr_core::gpu::GpuExt`. What the real system software does is known from
3dbrew and from how other emulators behave, not from measurement, so this
mode is never a reference for the operating system. It opens decrypted images
only and contains no keys.

## Process and memory (`memory.rs`, `horizon.rs`)

A process has a flat table of 4 KB pages, each with a physical page and
read/write/execute permissions; there are no guest page tables. The layout
follows 3dbrew's "Memory layout":

| Virtual | What |
|---|---|
| 0x00100000 | code, then read-only data, then data, each from a page |
| 0x08000000 | heap, grown by `ControlMemory` |
| up to 0x10000000 | the main thread's stack (0x4000 bytes) |
| 0x14000000 or 0x30000000 | linear heap, a window on the start of FCRAM |
| 0x1F000000 | VRAM (physical 0x18000000) |
| 0x1FF82000 | thread-local storage, 0x200 bytes a thread |

The application's part of FCRAM is 64 MB (an Old 3DS in its default mode).
Ordinary memory is taken from its top down and the linear heap from its
bottom up, so the linear heap is one run of physical memory, which is what
makes it usable by the GPU. Freed memory is not handed out again yet.

Applications run with unaligned access allowed; an access that straddles
pages is done a byte at a time. The exclusive monitor is per core on physical
words, and any store by the other core clears a mark, as on the low-level
machine. User code reads one coprocessor register, the thread-local storage
address (CP15 c13, c0, 3), and may issue the cache and barrier operations of
c7, which do nothing.

Until a program sets its own, the display controllers show RGB8 framebuffers
at the VRAM addresses a chainloader uses (0x18300000 and 0x18346500).

## Traps

`arm_core::Bus::hle()` makes the processor park supervisor calls, undefined
instructions, breakpoints and aborts for the host. A supervisor call is
answered (below); anything else, and a call that is not implemented, stops
the process with a report in `Horizon::fatal`: what it was, where, the last
faulting address and the link register.

## Threads and objects (`kernel.rs`; 3dbrew, "SVC" and "Multi-threading")

A thread has a priority from 0 (most urgent) to 63. The most urgent thread
that can run does, until it blocks, yields (`SleepThread(0)`) or a more
urgent one becomes ready; a pre-empted thread keeps its place, and threads of
one priority otherwise run in the order they became ready. All threads share
one processor so far: the processor number given to `CreateThread` is kept
and ignored until the second application core exists. Each thread has 0x200
bytes of thread-local storage, zeroed, up to eight pages of them; a new
thread starts with its argument in r0 and a zero link register.

Handles are small numbers from 1, reused when closed; 0xFFFF8000 and
0xFFFF8001 name the calling thread and the process. Objects are not freed.

A wait is over when its object is available to the thread: a signalled event
or timer, a mutex nobody else holds, a semaphore with a count, a thread that
has ended. Taking it clears a one-shot event, gives a mutex its owner (a
mutex counts how often its owner took it), or counts a semaphore down. When
an object becomes available its waiters are looked at by priority, then by
how long they have waited; a pulse event reaches all of them and is left
clear. A wait for all of several objects takes none until it can take all.
Results reach a blocked thread through its saved registers: the result code
in r0 and the index of the object in r1; a timeout gives 0x09401BFE. A
thread that ends releases its mutexes and wakes whoever waits for it.

The address arbiter wakes the most urgent waiters on an address first;
waiting compares the word at the address with the value as signed numbers and
the decrementing kinds store the word minus one before blocking.

Timers and timeouts are events on the shared clock, so they take effect at
the end of the quantum in which they fall due.

## Supervisor calls (`svc.rs`; 3dbrew, "SVC")

Arguments arrive in r0-r4 as libctru's veneers leave them; the result code
goes back in r0 and further results from r1.

| Number | Call | Behaviour |
|---|---|---|
| 0x01 | ControlMemory | Commit (ordinary at the address asked, within the heap range; linear at the end of the linear heap, the kernel choosing the address), free (unmaps), protect. Sizes and addresses are whole pages |
| 0x03 | ExitProcess | The process ends |
| 0x08 | CreateThread | Priority, entry, argument, stack top, processor |
| 0x09 | ExitThread | The thread ends |
| 0x0A | SleepThread | Nanoseconds to ARM11 cycles at 268,111,856 Hz, rounded down, at least one; zero yields |
| 0x0B, 0x0C | GetThreadPriority, SetThreadPriority | |
| 0x13, 0x14 | CreateMutex, ReleaseMutex | Releasing another thread's mutex is an error |
| 0x15, 0x16 | CreateSemaphore, ReleaseSemaphore | Returns the count before; past the maximum is an error |
| 0x17-0x19 | CreateEvent, SignalEvent, ClearEvent | Reset types one-shot, sticky, pulse |
| 0x1A-0x1D | CreateTimer, SetTimer, CancelTimer, ClearTimer | Initial delay and interval in nanoseconds |
| 0x21, 0x22 | CreateAddressArbiter, ArbitrateAddress | All five types |
| 0x23, 0x27 | CloseHandle, DuplicateHandle | |
| 0x24, 0x25 | WaitSynchronization1, WaitSynchronizationN | Any or all, with a timeout; negative waits for ever, zero polls |
| 0x28 | GetSystemTick | The clock: a tick is an ARM11 cycle |
| 0x35, 0x37 | GetProcessId, GetThreadId | The process is number 1 |
| 0x3C | Break | Stops the process with the reason |
| 0x3D | OutputDebugString | Into `Horizon::log` |

Result codes are composed from their fields (3dbrew, "Error codes"):
out of memory is 0xD86007F3, a misaligned size 0xE0E01BF2, an invalid
address 0xE0E01BF5.

## Time (`timing` constants in `horizon.rs`)

Time is ARM11 cycles. A thread runs for a quantum of 1024 cycles, then due
events are delivered, so a wake-up lands on a quantum boundary. When no thread
can run, time jumps to the quantum holding the next event, but not past the
end of the frame, and a frontend's frame runs to the next multiple of the
frame length on the clock. VBlank comes every 4,481,136 cycles, as on the low-level machine. The
quantum is arbitrary and pinned by the frame hashes; changing it is a timing
change.

## Not yet

The second application core, shared memory blocks, IPC and the services, game images, the
GX command queue, saves, the DSP. See the H milestones in `overview.md`.
