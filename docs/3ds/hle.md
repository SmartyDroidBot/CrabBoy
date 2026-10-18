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

## Loading (`loader.rs`; 3dbrew, "NCSD", "NCCH", "ExeFS", "RomFS")

A 3DSX is placed as homebrew expects. A card image (NCSD) or a bare NCCH
must carry the NoCrypto flag; anything encrypted is refused. The exheader
gives the three code segments, the stack size, the main thread's priority,
the memory mode and the kernel version the title was built for; `.code` is
reverse-LZSS-decompressed if the exheader says so. Titles built for kernel
2.44 or later get the linear heap at 0x30000000. The image is not copied:
the RomFS (level 3 of its hash tree) and ExeFS are windows on the medium,
read through `emu_core::Storage` as files are opened, so a 4 GB card image
costs nothing to load.

Configuration memory (0x1FF80000) and the shared page (0x1FF81000) are mapped
read-only. They say kernel and firmware 2.52 (SDK version 0xF297), a retail
unit, the memory region sizes for the title's mode (the three regions adding
up to 128 MB), no wireless network, the 3D slider down and the battery light
on. The shared page's clock is the date
2020-01-01 at the start, then moved only by emulated time, at each VBlank,
so a run is the same whatever the host's clock says.

New threads start with the VFP enabled (FPEXC.EN) and an FPSCR of
0x03C00000 (round to nearest, flush to zero, default NaN), which is what the
SDK's start-up code expects. Each thread switch clears the exclusive monitor,
as Horizon does with CLREX.

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

## Shared memory

A memory block is a list of physical pages: fresh, zeroed memory when it is
created at address zero, or the pages behind the address the creator gives.
Mapping it again maps the same pages, with the permissions asked for, or
with what the creator allows others when the caller does not care
(0x10000000). Address zero lets the kernel choose; it places blocks from
0x10000000 up with a free page between them. `QueryMemory` reports runs of
pages with the same permissions and state (free, static, code, private,
shared, continuous for the linear heap).

## IPC and services (`ipc.rs`, `service/`; 3dbrew, "IPC", "Services")

Services are host code that share the process's handle table and memory,
so a request needs no translation: a handle in it is the client's own, and a
buffer descriptor is followed by the client's own address, which a service
reads or writes in place. `SendSyncRequest` reads the message from the
command buffer (thread-local storage + 0x80), the service answers, and the
reply is written back with its own header. A session handle names a service
and, for services that hand out sessions of their own, one of its objects.

Two ports can be connected to by name: `srv:`, the service manager, and
`err:f`. `srv:` answers RegisterClient, EnableNotification (a semaphore
nothing signals), GetServiceHandle, the subscription commands (nothing is
published), ReceiveNotification (none) and IsServiceRegistered (always).
GetServiceHandle hands every name a session, without checking the
process's access list. A fatal error sent to `err:f` stops the process with
its type, result code, address and, for a failure, its message.

Services written so far:

| Service | What it answers |
|---|---|
| `APT:U` (and `:A`, `:S`) | The lock (a mutex), the notification and resume events, Enable queuing a Wakeup parameter from the home menu and signalling resume, Receive/GlanceParameter, startup argument (none), the CPU time limit, applet utility calls |
| `fs:USER` | The title's own archive (SelfNCCH): the RomFS and ExeFS files by name, read in place from the medium. Save data (4), extra data (6, 7) and the SD card (9, 10) are volumes of the save store, below |
| `gsp::Gpu` | The shared page (interrupt queue, framebuffer information, GX command queue), the GX commands carried out as writes to the GPU's registers, register reads and writes, buffer swaps, LCD black fill; completions relayed as interrupts after each job's cost; VBlank relayed to the queue |
| `hid:USER` (and `:SPVR`) | The shared page: buttons, circle pad and touch screen at 234 Hz, accelerometer at 104 Hz and gyroscope at 101 Hz when enabled (rates other emulators use, not measured), with the five events; gyroscope coefficient and calibration |
| `cfg:u` (and `:s`, `:i`) | Blocks of a console bought in the United States: English, USA, stereo, EULA agreed, no parental controls, an Old 3DS; a block not written out reads as zeros and is noted |
| `dsp::DSP` | A null DSP, below |

Memory a service shares with the application (GSP's and HID's pages) comes
from the system region of FCRAM, above the application's, so it does not
count against the application's memory.

The save store (`memfs.rs`) holds every volume a title writes: its save data
(once formatted with FormatSaveData), extra data (once created with
CreateExtSaveData) and the SD card (always there), each a tree of
directories and files, plus the secure values by slot. Until formatted, save
data answers "not formatted" (0xC8A04554) and missing extra data on the SD
card "not found" (0xC8A04478), as on a console that never held them. Writes
take effect at once. The store is `save_data()`'s bytes (magic `CRAB3DSF`,
version 1, little-endian), so frontends persist it as they persist any save.

The null DSP runs no firmware. `LoadComponent` says the firmware is loaded;
the audio pipe's Initialize command is answered with the fifteen shared
structures' addresses (16-bit words from 0x8000, laid out with room for what
3dbrew lists for each); then every 160 samples at 32,728 Hz (1,310,739
cycles) the next frame count is written to the shared region of its parity
(0x1FF50000 or 0x1FF70000) and the audio pipe's interrupt event is signalled.
It produces silence. DSP memory (0x1FF00000, 512 KB) is mapped read/write.

A service that is not written yet is a stub: every command gets success and
zeros, and the first call of each command is logged with its words. A
strict console (`Horizon::strict`) stops the process there instead.

## Supervisor calls (`svc.rs`; 3dbrew, "SVC")

Arguments arrive in r0-r4 as libctru's veneers leave them; the result code
goes back in r0 and further results from r1.

| Number | Call | Behaviour |
|---|---|---|
| 0x02 | QueryMemory | Base, size, permissions and state of the run around an address; no page flags |
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
| 0x1E-0x20 | CreateMemoryBlock, MapMemoryBlock, UnmapMemoryBlock | See "Shared memory" |
| 0x21, 0x22 | CreateAddressArbiter, ArbitrateAddress | All five types |
| 0x23, 0x27 | CloseHandle, DuplicateHandle | |
| 0x24, 0x25 | WaitSynchronization1, WaitSynchronizationN | Any or all, with a timeout; negative waits for ever, zero polls |
| 0x28 | GetSystemTick | The clock: a tick is an ARM11 cycle |
| 0x2A | GetSystemInfo | Memory in use (type 0), 0 for type 2, 5 processes for type 26 |
| 0x2B | GetProcessInfo | Memory in use (0, 2), handles (4, 5), threads (7), the linear heap's offset from FCRAM (20); other types are "not available", 0xD8E007ED |
| 0x2D | ConnectToPort | `srv:` and `err:f` |
| 0x32 | SendSyncRequest | See "IPC and services" |
| 0x35, 0x37 | GetProcessId, GetThreadId | The process is number 1 |
| 0x38-0x3A | GetResourceLimit, its limits and current values | An application's limits: priority 0x18, its memory, 32 threads, events and mutexes, 8 semaphores and timers, 16 shared memory blocks, 2 arbiters |
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

## Diagnostics

`Horizon::trace` holds flags by area (`trace.rs`: svc, ipc, gx, irq, dsp, ro,
fs, service); code notes details of an area only when its flag is set, and
`ctr-diag --trace=gx,irq` sets them. Every registered service hears of each
vertical blank through `Service::vblank`.

## Not yet

The second application core, `ldr:ro` (CRO modules), the GPU drawing what
command lists ask (the `pica` pipeline), the DSP running its firmware, most
other services. See the H milestones in `overview.md`.
