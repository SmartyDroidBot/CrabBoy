# 3DS services in the high-level mode

What each system service of `crates/ctr-hle` answers, and why. The
mechanics (sessions, the command buffer, the stub) are in `hle.md`; this
page is the catalogue. Command layouts come from 3dbrew (each service's
page) and libctru's headers; what a service answers is what a retail
Old 3DS bought in the United States answers when it has never been online,
has no StreetPass partner, no Circle Pad Pro, no amiibo and no camera
subject: the console a game expects to be able to run on, alone.

## Ground rules

- A command whose answer the game depends on is written out. A command
  that only needs to succeed is answered from a table (`table.rs`): success,
  fixed words, and real kernel events where the command hands out events.
  An event is made the first time it is asked for and handed out again
  after that; it is never handle 0.
- An event that announces something that never happens here (a network
  connection, a StreetPass, a new SpotPass download) is never signalled.
  An asynchronous request that cannot succeed here (a login, a connection)
  ends at once: its event is signalled and its result is a failure.
- A failure code no measurement gives is composed from the fields 3dbrew
  documents (level, summary, module, description), and each one is named
  as such where it is defined.
- A command no table or service knows goes to the stub: success and
  zeros, logged once, or the end of the process on a strict console.

## The services of Pokemon Ultra Sun

The game's exheader lists 33 services. Each one, and what it answers:

| Service | Module | What it answers |
|---|---|---|
| `APT:A` | `apt.rs` | See `hle.md` |
| `fs:USER` | `fs.rs` | See `hle.md` |
| `gsp::Gpu` | `gsp.rs` | See `hle.md` |
| `hid:USER` | `hid.rs` | See `hle.md` |
| `cfg:u` | `cfg.rs` | See `hle.md` |
| `dsp::DSP` | `dsp.rs` | See `hle.md` |
| `ldr:ro` | `ldr_ro.rs` | The CRO linker (`cro.md`) |
| `ndm:u` | `ndm.rs` | Exclusive state, daemon masks (all four by default), suspended daemons (status idle or suspended), scheduler, scan interval (30 s) and retry interval: each reads back as it was set; the network state is always the initial one |
| `ptm:u` | `ptm.rs` | Adapter plugged in, shell open, battery level 5 (full), charging; pedometer not counting and no steps in any hour; the RTC alarm and pedometer recording mode read back |
| `ac:u` | `ac.rs` | A default configuration of zeros; configuration edits hand the configuration back; ConnectAsync signals its event at once and GetConnectResult fails with 0xE0A09D2E (not connected); status and Wi-Fi status 0; every question about the connected access point fails the same way; IsConnected 0 |
| `act:u` | `act.rs` | One console account in slot 1 with no Nintendo Network ID: persistent id 0x80000001, principal id 0, empty account id, Mii name "CrabBoy", country "US", born 2000-01-01; account server requests signal their event and fail (0xD8A14FF8) |
| `am:app` | `am.rs` | No downloadable content or patch is installed: DLC and patch title questions fail with 0xD88083FA (title not found), ticket counts are 0, nothing is in use |
| `boss:U` | `boss.rs` | Sessions, storage (SetStorageInfo, RegisterStorageEntry, storage options read back), the opt-out flag (reads back), tasks that register, start and never run, empty NS data lists, a task finish event that never fires |
| `cecd:u` | `cecd.rs` | Box files a title writes (by program id and path type) read back for the life of the console; messages never arrive (ReadMessage fails, not found 0xD8810BFA); the daemon is idle; its two events never fire |
| `frd:u` | `frd.rs` | Not logged in, not online; no principal id or friend code yet; screen name "CrabBoy"; profile USA/English; playing this program; an empty friend list and empty records for any friend asked about; Login and the server requests signal their event and fail (0xD8A0C7F8) |
| `y2r:u` | `y2r.rs` | The converter in full, below |
| `news:u` | `simple.rs` | Notifications are accepted and go nowhere; there are none |
| `nim:aoc` | `simple.rs` | Initialised; balance and support code 0 |
| `dlp:SRVR` | `simple.rs` | Not a Download Play child (IsChild 0); a server nobody joins |
| `dlp:FKCL` | `simple.rs` | Scans that find nothing |
| `qtm:u` | `simple.rs` | New 3DS head tracking: no face, all zeros; not blacklisted |
| `pxi:dev` | `simple.rs` | PublishToSubscriber succeeds |
| `$hioFIO`, `$hostio0`, `$hostio1` | `simple.rs` | Development host I/O: a retail title never opens them; any command is the stub's |

## Y2R (`y2r.rs`; 3dbrew, "Camera Services", y2r:u)

The game's intro is a Mobiclip video: the decoder makes YUV pictures and
Y2R turns them into RGB textures, and the game waits on Y2R's transfer-end
event for each. Every setting is kept and reads back (input and output
format, rotation, block alignment, line width and count, coefficients,
the standard coefficient, alpha, dithering, the package parameter of
SetPackageParameter: formats, rotation and alignment in one word, width
and lines in the next, standard coefficient and alpha in the third).

- Input: YUV 4:2:2 or 4:2:0 in three planes of 8-bit samples, or of
  16-bit samples with the value in the low byte; or 4:2:2 as one batch of
  Y0 U Y1 V. Chroma is shared by each pair of pixels, and in 4:2:0 by each
  pair of lines.
- Each plane, and the output, is a DMA transfer of `size` bytes moved
  `unit` bytes at a time with a gap of `gap` bytes skipped after each unit.
- The conversion: with the coefficients' eight fraction bits and the
  offsets' three more, `R = ((Y*Y_A + V*R_V) >> 3) + R_Offset`,
  `G = ((Y*Y_A - V*G_V - U*G_U) >> 3) + G_Offset`,
  `B = ((Y*Y_A + U*B_U) >> 3) + B_Offset`, then each channel
  `(c + 0x18) >> 5` clamped to a byte. The rounding term is the one other
  emulators matched against captures of the hardware. The four standard
  coefficient sets are BT.601 and BT.709 for full-range and for
  studio-range samples (GetStandardCoefficientParams returns them).
- Output: RGBA8 (bytes A, B, G, R), RGB8 (B, G, R), RGB5A1 (alpha's top
  bit) or RGB565, little-endian.
- The picture goes through in strips of eight lines. In lines, each row of
  the strip follows the last; in 8x8 blocks, the strip's tiles follow each
  other left to right, each tile's pixels in Morton order (x in the low
  bit), as GPU textures are laid out. A rotation turns each strip (a
  quarter turn makes it eight pixels wide), and the turned strip is then
  laid out the same way.
- Not done: dithering (it only changes the lowest bits of the 16-bit
  formats).

StartConversion converts the whole picture, marks the planes sent and the
output received, and signals the transfer-end event if SetTransferEndInterrupt
enabled it.
