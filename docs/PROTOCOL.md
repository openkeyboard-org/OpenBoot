# OBP v0.1 — the OpenBoot wire protocol

This is the normative specification of the OpenBoot Protocol ("OBP"),
version 0.1 (`OB_PROTO_MAJOR = 0x00`, `OB_PROTO_MINOR = 0x01`). It is
self-contained: a third-party client can be implemented from this document
alone. Where this document and
[`protocol/openboot_protocol.h`](../protocol/openboot_protocol.h) disagree,
the header wins and the disagreement is a documentation bug; the constants
below are quoted from that header. The test vectors in
[`protocol/golden_frames.txt`](../protocol/golden_frames.txt) are normative —
an implementation that cannot round-trip them is wrong.

"MUST", "SHOULD", and "MAY" are used in their customary normative sense.
"Device" is the bootloader; "host" is the flashing client.

## 1. Conventions

- All multi-byte integers are **little-endian**, everywhere: frame CRC,
  payload fields, the boot record, the RAM boot-request word.
- Byte offsets are zero-based. Payload offsets are relative to the start of
  the payload (frame offset 4), not the frame.
- Hex values are written `0xNN`; on-wire byte sequences are written as
  space-separated hex pairs in transmission order.

## 2. Logical frame

One logical frame format is used in both directions on both transports:

```text
offset  size  field
0       1     cmd      request 0x01..0x7F; response = request | 0x80;
                       0xFF = frame-error report
1       1     seq      opaque, chosen by the host, echoed by the device
2       1     len      payload length N, 0..56 (OB_MAX_PAYLOAD = 0x38)
3       1     flags    MUST be 0 in v0.1; nonzero is rejected with E_ARG
4       N     payload  command-specific (section 6)
4+N     4     crc32    CRC-32/ISO-HDLC over bytes [0, 4+N), little-endian
```

| Constant | Value | Meaning |
|---|---|---|
| `OB_FRAME_HDR_LEN` | `0x04` | header bytes covered before payload |
| `OB_FRAME_CRC_LEN` | `0x04` | trailing CRC bytes |
| `OB_FRAME_OVERHEAD` | `0x08` | header + CRC |
| `OB_MAX_PAYLOAD` | `0x38` (56) | maximum `len` |
| `OB_MAX_FRAME` | `0x40` (64) | maximum total frame = one HID report |
| `OB_MAX_WRITE_DATA` | `0x30` (48) | maximum data bytes in one WRITE |

Rules:

- A frame is 8–64 bytes total. `len > 56` is not a legal frame.
- The CRC covers **everything before it**: `cmd`, `seq`, `len`, `flags`, and
  the payload.
- Responses set bit 7 of the request opcode (`OB_CMD_RESP_BIT = 0x80`) and
  echo `seq` unchanged.
- `seq` is opaque to the device: it is echoed, never interpreted, and the
  device performs **no deduplication** of retried requests. The host SHOULD
  use a fresh `seq` for every transmission (including retries) and MUST
  discard any response whose `seq` does not match the outstanding request.
- Strict ping-pong: the host MUST NOT transmit a new request until it has
  received the response to the previous one or timed out. The device never
  sends unsolicited frames.

### 2.1 Frame-error report (`0xFF`)

When the device receives a locatable candidate frame whose CRC does not
match, it answers with a frame-error report instead of a command response:

```text
cmd = 0xFF (OB_CMD_FRAME_ERR)   seq = best-effort echo   len = 2   flags = 0
payload = [OB_E_CRC, 0x00]
```

The echoed `seq` comes from a frame that failed its integrity check, so it
is **best-effort only**; because of strict ping-pong the host MUST treat any
`0xFF` report as the failure of its outstanding request regardless of `seq`,
and retry with a fresh `seq`. A frame-error report never changes device
state.

An **undecodable** frame — the `len` byte exceeds `OB_MAX_PAYLOAD`, or fewer
than `4 + len + 4` bytes are in hand — is dropped **silently on both
transports** (the header cannot be trusted, so no report is derived from
it): the UART parser re-hunts for SOF (section 4.2) and the USB path
discards the report. The host recovers by timeout and retry, exactly as for
a lost response.

## 3. CRC-32 parameters

OBP uses CRC-32/ISO-HDLC — the CRC of zlib, Ethernet, and PNG — for the
frame CRC, the image CRC, the boot-record CRC, and the session stream CRC.

| Parameter | Value |
|---|---|
| Polynomial | `0x04C11DB7` (reflected form `0xEDB88320`) |
| Initial value | `0xFFFFFFFF` |
| Reflect input / output | yes / yes |
| Final XOR | `0xFFFFFFFF` |
| Check value | `crc32("123456789") = 0xCBF43926` |

The check value is pinned as the first golden vector
(`crc_check: 2639f4cb` — note the little-endian storage). Any zlib-style
`crc32()` (Python `zlib.crc32`, Rust `crc32fast`) produces these values with
no configuration.

## 4. Transport mappings

The logical frame is byte-identical on both transports; only the container
differs. A device reports which transport it is built for in the HELLO
response (`transport`: `OB_TRANSPORT_ID_USB = 0x01`,
`OB_TRANSPORT_ID_UART = 0x02` — one per firmware image, never both).

### 4.1 USB (HID)

- Every logical frame occupies **exactly one 64-byte interrupt report**,
  zero-padded to 64 bytes, in both directions. One report = one frame; there
  is no fragmentation or continuation.
- Requests travel host→device on the interrupt OUT endpoint; responses
  device→host on the interrupt IN endpoint (see section 12 for the
  descriptor summary).
- The device declares **no report IDs**. Per the USB HID convention, hosts
  using hidapi MUST therefore prepend a `0x00` report-ID byte to each write
  (a 65-byte `hid_write` whose remaining 64 bytes are the padded report);
  reads return the bare 64-byte report with no report-ID byte.
- The `len` byte locates the payload and CRC inside the report. Padding
  bytes after the CRC MUST be sent as zero and MUST be ignored on receipt.

### 4.2 UART

On-wire encoding: a 2-byte start-of-frame prefix, then the logical frame
bytes. There is no end-of-frame marker and no byte escaping.

```text
0xB0 0x07  cmd seq len flags payload... crc32
```

| Constant | Value |
|---|---|
| `OB_UART_SOF1` | `0xB0` |
| `OB_UART_SOF2` | `0x07` |
| `OB_UART_BAUD` | 115200 (8N1, no flow control) |
| `OB_UART_INTERBYTE_MS` | `0x32` (50) |

Receive parser rules (both sides implement the same state machine):

1. **Hunt** for `0xB0`. The next byte must be `0x07`; otherwise return to
   hunting (implementations SHOULD treat a second `0xB0` as a fresh SOF1
   candidate).
2. Read the 4-byte header. If `len > OB_MAX_PAYLOAD (56)`, the frame is
   **silently dropped** and the parser re-hunts — no frame-error report is
   possible because the parser cannot know where the bogus frame ends.
3. Read `len` payload bytes and the 4-byte CRC, then deliver the candidate
   frame for CRC validation (device: bad CRC ⇒ `0xFF` report; host: bad
   CRC ⇒ treat as a failed exchange and retry).
4. A **mid-frame gap longer than 50 ms** between consecutive bytes resets
   the parser to hunting. This resynchronizes framing only — it never
   touches the session (bitmap, stream CRC, boot record are unaffected).

SOF bytes inside a payload are ordinary data. The length, frame CRC,
strict ping-pong exchange, and inter-byte timeout provide resynchronization;
there is no escaping.

## 5. Session model

The device has two states. In IDLE, only HELLO is accepted; other commands
return `E_STATE`. A successful HELLO enters SESSION, where the full command
set is available. There is no session timeout or return to IDLE except reset.

| Event | Effect |
|---|---|
| Power-on / any reset | State = IDLE; idle auto-boot timer starts if a valid boot record exists |
| Successful HELLO | State = SESSION; bitmap and stream CRC reset; auto-boot disabled until reset |
| Pre-mutation error or frame-error report | No state change |
| `E_FLASH` during mutation | Fail-safe partial state described in section 7 |
| UART inter-byte gap > 50 ms | RX parser resynchronized only; session untouched |
| First ERASE or WRITE | Boot record invalidated before the mutation |
| COMMIT with status OK | New tuple: boot record written; exact successful replay: existing record remains valid without another write |
| BOOT mode 0 | Respond, stop transport, reset, and launch the app through the boot decision |
| BOOT mode 1 | Respond, set the RAM request, and reset back into OpenBoot |

## 6. Command reference

Opcodes:

| Opcode | Name | Request payload | Success response payload | Session required |
|---|---|---|---|---|
| `0x01` | HELLO | 6 B | ≥ 36 B | no (creates one) |
| `0x02` | ERASE | 8 B | 1 B | yes |
| `0x03` | WRITE | 8..52 B | 1 B | yes |
| `0x04` | CRC | 8 B | 5 B | yes |
| `0x05` | COMMIT | 8 B | 1 B | yes |
| `0x06` | BOOT | 1 B | 1 B | yes |
| `0x07` | READ | reserved | reserved | yes |

Response payload conventions:

- Byte 0 of every response payload is a **status** byte (section 7).
- On success (`OB_OK = 0x00`) the status is followed by any
  command-specific result data.
- On failure the payload is always exactly 2 bytes: `[status, detail]`.
  `detail` is `0x00` (`OB_DET_NONE`) unless the status defines detail codes.
- A request whose payload length does not match the command's requirement
  is answered with `E_LEN`.

### 6.1 HELLO (`0x01`)

Opens (or reopens) a session and describes the device. The host learns
everything it needs from this response — app-region bounds, granularities,
chip identity, unique ID — and MUST NOT rely on a chip database.

Request payload (`OB_HELLO_REQ_LEN = 0x06`):

| Offset | Size | Type | Field | Value |
|---|---|---|---|---|
| 0 | 4 | u32 | magic | `OB_HELLO_MAGIC = 0x3150424F` — the ASCII bytes `"OBP1"` (`4F 42 50 31` on the wire) |
| 4 | 1 | u8 | host_major | host protocol major, `0x00` |
| 5 | 1 | u8 | host_minor | host protocol minor, `0x01` |

Success response payload (`OB_HELLO_RESP_LEN = 0x24` = 36 bytes; the device
MUST send at least this much, the host MUST tolerate more — section 11):

| Offset | Size | Type | Field | Meaning |
|---|---|---|---|---|
| 0 | 1 | u8 | status | `OB_OK` |
| 1 | 1 | u8 | proto_major | `0x00` |
| 2 | 1 | u8 | proto_minor | `0x01` |
| 3 | 1 | u8 | chip_rev | ROM configuration chip-id byte |
| 4 | 2 | u16 | bl_version | bootloader version (mirrored in USB `bcdDevice`) |
| 6 | 1 | u8 | chip_family | `OB_FAMILY_*`, table below |
| 7 | 1 | u8 | transport | `0x01` USB, `0x02` UART |
| 8 | 4 | u32 | app_start | first writable flash address (`0x00002000` on all supported chips) |
| 12 | 4 | u32 | app_end | exclusive end of the app region |
| 16 | 4 | u32 | erase_block | erase granularity in bytes (4096) |
| 20 | 2 | u16 | write_page | flash write page in bytes (256, informational) |
| 22 | 1 | u8 | write_align | WRITE address/data alignment (4) |
| 23 | 1 | u8 | max_write_data | maximum data bytes per WRITE (48) |
| 24 | 4 | u32 | features | `OB_FEAT_*` bit set, table below |
| 28 | 8 | u64 | uid | 64-bit ROM unique ID |

| `chip_family` | Chip |
|---|---|
| `OB_FAMILY_CH570 = 0x01` | CH570 |
| `OB_FAMILY_CH572 = 0x02` | CH572 |
| `OB_FAMILY_CH591 = 0x03` | CH591 |
| `OB_FAMILY_CH592 = 0x04` | CH592 |

| Feature bit | Value | Meaning |
|---|---|---|
| `OB_FEAT_READ` | `0x01` | the READ command is available |
| `OB_FEAT_CRC_LIVE` | `0x02` | CRC results are authoritative even for flash written this power cycle. **Clear on CH57x**, where XIP reads after controller writes may serve stale data; when clear, the host MUST NOT trust a CRC over any region written since the last reset (section 6.4) |

Errors: `E_LEN` (payload not 6 bytes), `E_ARG` (bad magic), and `E_PROTO`
(unsupported version).

Semantics: a successful HELLO always (re)initializes the session — the
erased-block bitmap is cleared and the stream-CRC tracker reset, whether or
not a session was already open — and permanently disables idle auto-boot
for this power cycle (section 10).

### 6.2 ERASE (`0x02`)

Request payload (8 bytes):

| Offset | Size | Type | Field | Constraint |
|---|---|---|---|---|
| 0 | 4 | u32 | addr | multiple of `erase_block` (4096) |
| 4 | 4 | u32 | len | multiple of `erase_block`, nonzero |

`[addr, addr + len)` MUST lie entirely within `[app_start, app_end)`;
violations are answered with `E_ADDR` (detail: range or align) before any
flash is touched.

Behavior: if this is the session's first mutation, the boot record is
invalidated first (section 8). The device then erases block by block in one
4096-byte ROM call per block, marking each block in the erased-block bitmap
as it succeeds. A ROM failure aborts the loop with `E_FLASH` (detail: low
byte of the ROM return code); blocks erased before the failure remain
marked. ERASE is idempotent — the host recovers from `E_FLASH` or a lost
response by simply re-issuing the same command.

An ERASE that overlaps the region already folded into the session stream
CRC (section 6.3) voids the stream: the CRC would describe bytes that no
longer exist. On devices without `FEAT_CRC_LIVE` a subsequent COMMIT then
fails with `E_VERIFY`/nonseq; re-erasing is only stream-safe *before* the
first WRITE of the run.

Success response: 1 byte, `[OB_OK]`.

Host guidance: chunk large erases into requests of ≤ 32 KiB (8 blocks) for
progress reporting and sane per-request timeouts.

### 6.3 WRITE (`0x03`)

Request payload (8–52 bytes):

| Offset | Size | Type | Field | Constraint |
|---|---|---|---|---|
| 0 | 4 | u32 | addr | multiple of `write_align` (4) |
| 4 | 4..48 | bytes | data | length a multiple of 4, at least 4, at most `max_write_data` (48) |

Checks, in order:

1. Payload length in `[8, 52]` and `(payload_len - 4)` a multiple of 4,
   else `E_LEN`.
2. `[addr, addr + data_len)` within `[app_start, app_end)` and `addr`
   4-aligned, else `E_ADDR`.
3. **Every 4096-byte block the write touches MUST be marked in the
   erased-block bitmap** (i.e. erased earlier in this same session), else
   `E_NOT_ERASED`. This is the structural fix for the vendor
   write-before-erase bug: no write can land anywhere an erase was not
   explicitly performed first, and the bootloader region can never be in
   the bitmap because ERASE refuses it.

The device programs the data (`FLASH_ROM_WRITE`) and immediately verifies
it through the flash controller (`FLASH_ROM_VERIFY` — a controller
read-back, deliberately not an XIP read; see `FEAT_CRC_LIVE`). Either ROM
call failing yields `E_FLASH` with the low byte of the ROM return code as
detail.

Success response: 1 byte, `[OB_OK]`.

**Stream CRC.** The device maintains a sequential-run tracker for COMMIT on
devices without `FEAT_CRC_LIVE`: the run starts at `app_start`; a WRITE at
exactly the current run position folds its data into the session stream CRC
and advances the position. A **byte-exact** re-send of the immediately
previous chunk (same address, length, and data — the lost-response retry)
is acknowledged without folding it again. A successful WRITE anywhere
else — a different range, or the same range with *different* bytes (e.g.
further 1→0 programming the flash controller accepts) — permanently breaks
the run for the remainder of the session, as does an ERASE that overlaps
the already-streamed region (section 6.2). Image data SHOULD therefore be
written as one strictly sequential pass from `app_start`. Writes are
idempotent at the flash level (same data into erased-and-verified cells),
so retrying after a lost response is always flash-safe; if the run was
broken on a device without `FEAT_CRC_LIVE`, COMMIT fails closed with
`E_VERIFY` (nonseq or mismatch) and the host restarts the update.

### 6.4 CRC (`0x04`)

Request payload (8 bytes):

| Offset | Size | Type | Field | Constraint |
|---|---|---|---|---|
| 0 | 4 | u32 | addr | multiple of 4 |
| 4 | 4 | u32 | len | multiple of 4, nonzero |

`[addr, addr + len)` MUST lie within `[app_start, app_end)`, else `E_ADDR`.
The bootloader region is not CRC-addressable — readback oracles included.

Success response (5 bytes):

| Offset | Size | Type | Field |
|---|---|---|---|
| 0 | 1 | u8 | status = `OB_OK` |
| 1 | 4 | u32 | CRC-32/ISO-HDLC of the region, read via XIP |

Trust rule: if `OB_FEAT_CRC_LIVE` is set in HELLO, the result is
authoritative unconditionally. If it is clear (CH57x), XIP reads of flash
written **this power cycle** may return stale data, so the host MUST NOT
use CRC to judge freshly written flash — that is COMMIT's job (which uses
the stream CRC there). CRC over regions not written since the last reset is
trustworthy on all devices.

### 6.5 COMMIT (`0x05`)

Attests the flashed image and, on success, writes the boot record — the
only operation that makes an image bootable.

Request payload (8 bytes):

| Offset | Size | Type | Field | Constraint |
|---|---|---|---|---|
| 0 | 4 | u32 | img_len | multiple of 4, nonzero, `img_len ≤ app_end − app_start` |
| 4 | 4 | u32 | img_crc32 | CRC-32/ISO-HDLC over `[app_start, app_start + img_len)` |

An out-of-range `img_len` is answered with `E_ADDR` (detail: range).

Attestation path:

- **`FEAT_CRC_LIVE` set (CH59x):** the device computes the CRC directly
  over `[app_start, app_start + img_len)` and compares with `img_crc32`.
- **`FEAT_CRC_LIVE` clear (CH57x), session performed writes:** the session
  stream CRC is used instead. The writes MUST have formed one unbroken
  sequential run starting at `app_start` covering exactly `img_len` bytes;
  otherwise the device answers `E_VERIFY` with detail
  `OB_DET_VERIFY_NONSEQ`. A covered-length or CRC mismatch yields
  `E_VERIFY` with detail `OB_DET_VERIFY_MISMATCH`.
- **`FEAT_CRC_LIVE` clear, no flash mutation since power-on (the "bless"
  path):** XIP is coherent only until the first controller write of the
  power cycle, so this path is available only while the device has
  performed **no ERASE, WRITE, or record write since reset** — the state
  survives re-HELLO (a new session cannot restore XIP coherence) and an
  ERASE alone closes it. The device CRCs flash directly, as in the first
  path. This lets a host attest and boot-enable an image that was flashed
  out-of-band (e.g. via SWD) without rewriting it.

On match the device writes the boot record (section 8) and answers
`[OB_OK]`; a record storage failure is `E_FLASH`. On mismatch **no record
is written** — the device never converts an unverified image into a
bootable one, and a previously invalidated record stays invalid.

COMMIT is retry-idempotent. After a successful record write, the device
remembers the committed `(img_len, img_crc32)` tuple until reset or the next
flash mutation. An exact replay, including after another HELLO, answers
`[OB_OK]` without re-attesting or rewriting the record. A different tuple
follows the normal attestation path. This rule is required for the CH57x
bless path, where the first record write makes XIP unsuitable for a second
attestation in the same power cycle.

### 6.6 BOOT (`0x06`)

Request payload (1 byte):

| Offset | Size | Type | Field | Values |
|---|---|---|---|---|
| 0 | 1 | u8 | mode | `OB_BOOT_APP = 0x00`, `OB_BOOT_STAY = 0x01`; anything else `E_ARG` |

**Mode 0 (boot the app):** the device validates the boot record, using its
in-RAM knowledge in preference to a flash read where the two can disagree:
from the moment a record is invalidated (first mutation of a session)
until a COMMIT succeeds, BOOT is refused with `E_VERIFY` /
`OB_DET_VERIFY_NORECORD` even if a stale (F26) flash view still shows the
old record; conversely a COMMIT that succeeded this power cycle satisfies
the check even if the stale view does not yet show the new record. If no
mutation has occurred since reset, the device applies the **full
boot-decision validation** — record integrity, the erased-first-word
check, and the optional `OB_BOOT_IMAGE_CRC` image check — so an explicit
BOOT can never launch an app the reset path would refuse. On failure it
answers `E_VERIFY` with detail `OB_DET_VERIFY_NORECORD` and remains in
the session.

A passing mode-0 BOOT is **always reset-to-launch, on every family**: the
device sends `[OB_OK]` **first**, drains the transmit path (UART: TX FIFO
plus one character time; USB: the IN report completes), tears the
transport down (USB: SIE reset and D+ pull-up released, then a 10 ms
settle so the host sees a clean detach), and performs a **chip reset with
no boot-request magic written** — so the post-reset boot decision
(section 10), the single launch authority in the device, validates with
coherent flash and starts the application. On CH57x the reset is also
what makes freshly written code safe to execute (the CPU fetches through
XIP, which can serve stale pre-update bytes until reset — F26). The
response is deliberately sent before the reset so the host always gets a
definitive answer; the only cost versus a hypothetical direct jump is a
few milliseconds of boot.

**Mode 1 (reset, stay in bootloader):** the device sends `[OB_OK]`, writes
the RAM boot-request magic (section 9) so the boot decision keeps it in the
bootloader, and performs a chip reset.

BOOT is the one command a host MUST NOT retry: a lost response is
indistinguishable from a successful jump/reset, and the device may already
be gone. Re-probe instead.

### 6.7 READ (`0x07`) — reserved

Reserved for a future readback command; ships disabled. A device without
`OB_FEAT_READ` in its HELLO features answers `E_CMD`, which the host MUST
treat as "not supported" (this is the standard probing path, section 11).
The request/response payloads will be defined by the protocol minor that
first ships the feature.

## 7. Error taxonomy

Failure responses always carry the 2-byte payload `[status, detail]`.
**Errors detected before any mutation leave protocol state unchanged** —
bad lengths, addresses, arguments, states, and CRC failures never touch
the session, bitmap, stream CRC, or boot record. An `E_FLASH` from a
mutating command, however, may follow a *partial* mutation, always in the
fail-safe direction: ERASE keeps the marks for blocks it erased before
the failure, the session's boot record may already have been invalidated,
and the stream CRC may already have been poisoned (a mutation that fails
midway can never leave the stream attestable). Hosts recover with the
command-specific retry semantics (re-issue ERASE; restart the sequential
run for WRITE).

| Status | Value | Meaning | Detail byte |
|---|---|---|---|
| `OB_OK` | `0x00` | success | — |
| `OB_E_CRC` | `0x01` | frame CRC mismatch (arrives via the `0xFF` report) | `0x00` |
| `OB_E_LEN` | `0x02` | payload length invalid for the opcode | `0x00` |
| `OB_E_CMD` | `0x03` | unknown or unavailable opcode | `0x00` |
| `OB_E_STATE` | `0x04` | command requires a session (no HELLO yet) | `0x00` |
| `OB_E_ARG` | `0x05` | bad magic, bad BOOT mode, or nonzero flags | `0x00` |
| `OB_E_ADDR` | `0x06` | address/range violation | `OB_DET_ADDR_RANGE = 0x01` (outside the app region), `OB_DET_ADDR_ALIGN = 0x02` (misaligned) |
| `OB_E_NOT_ERASED` | `0x07` | WRITE into a block not erased this session | `0x00` |
| `OB_E_FLASH` | `0x08` | flash ROM API failure | low byte of the ROM return code |
| `OB_E_VERIFY` | `0x09` | attestation/record failure | `OB_DET_VERIFY_MISMATCH = 0x01`, `OB_DET_VERIFY_NONSEQ = 0x02`, `OB_DET_VERIFY_NORECORD = 0x03` |
| `OB_E_PROTO` | `0x0A` | unsupported protocol major in HELLO | `0x00` |

`OB_DET_NONE = 0x00` is the detail for every status that defines no
specific codes.

## 8. Safety model

The protocol is built so that **no sequence of host requests — valid,
invalid, malicious, or truncated by power loss — can brick the device.**

1. **The bootloader region `[0x0, 0x2000)` does not exist to the
   protocol.** One shared range validator gates ERASE, WRITE, and CRC; all
   three see only `[app_start, app_end)`. The region cannot be erased,
   written, or even CRC'd.
2. **Arming: the erased-block bitmap.** A 16-byte RAM bitmap
   (`OB_BITMAP_BYTES = 16`, one bit per 4 KiB block from `app_start`;
   CH592's 440 KiB region needs 110 bits) records which blocks this
   session has successfully erased. WRITE refuses (`E_NOT_ERASED`) any
   block not armed. The bitmap is cleared at reset and on every HELLO.
   This structurally removes the vendor USB-IAP bug where a write could
   land — including over the bootloader — before any erase.
3. **Disarm before mutation.** The first ERASE or WRITE of each session
   invalidates the boot record *before* the mutation executes. From that
   moment until COMMIT succeeds there is no valid record, so power loss at
   any instant of an update leaves a device that boots into the
   bootloader and can simply be re-flashed. An interrupted update is an
   inconvenience, never a brick.
4. **COMMIT is the only gate to bootability**, and it only opens on a CRC
   match (section 6.5). There is no path that records an unverified image.
5. **No self-update.** OBP v0.1 cannot modify the bootloader, including
   deliberately. Bootloader updates go through the chip's mask-ROM ISP
   (`wchisp`, BOOT strap) or SWD (`minichlink`) — both always available
   regardless of what OBP has done, which is precisely why v1 omits the
   feature.

What the protocol does **not** provide:

- **No authentication, encryption, or signing.** Any host with transport
  access can flash the device; OBP treats transport access as equivalent to
  physical access. Add link- or image-level security at the application
  layer if the deployment requires it.
- **No readback** in the shipped configuration (READ is reserved and
  feature-gated off). CRC is the only oracle over flash contents.
- **No pipelining** — strict ping-pong, one outstanding request.
- **No UART auto-baud** and no baud negotiation: fixed 115200 8N1.
- **No multi-image or slot management**: one app region, one record.

## 9. Boot record and RAM boot request

### 9.1 Boot record

The boot record is the persistent statement "a verified image of this
length and CRC is installed". It is written only by COMMIT and invalidated
at the first mutation of every session.

16-byte layout (`ob_boot_record_t`, all fields little-endian):

| Offset | Size | Field | Meaning |
|---|---|---|---|
| 0 | 4 | magic | `OB_RECORD_MAGIC = 0x3152424F` — ASCII `"OBR1"` (`4F 42 52 31` in memory) |
| 4 | 4 | img_len | image bytes from `app_start`, multiple of 4 |
| 8 | 4 | img_crc32 | CRC-32/ISO-HDLC over `[app_start, app_start + img_len)` |
| 12 | 4 | rec_crc32 | CRC-32/ISO-HDLC over the 12 bytes above |

A record is valid iff the magic matches, `rec_crc32` matches, and
`img_len` is a nonzero multiple of 4 that fits the app region.

Storage location (one 4096-byte block, outside the app region on every
chip, so ordinary app flashing never touches it):

| Family | Location |
|---|---|
| CH570 / CH572 | code flash `0x3B000` — the block immediately after the app region (no DataFlash on these chips) |
| CH591 / CH592 | DataFlash offset `0x7000` — the **last** 4 KiB block of the 32 KiB DataFlash, leaving the bottom free for applications |

**What the record does not say.** It describes exactly `img_len` bytes from
`app_start` and says nothing about the flash beyond them. Within an OBP session
that distinction never surfaces, because the record is invalidated at the first
mutation and only COMMIT writes a new one. It surfaces when an image is written
**out of band** — over SWD or ROM ISP — and an older record survives: the
bootloader recomputes the CRC over the recorded length, so the record still
validates whenever the newly written bytes have the recorded image as a
*prefix*. Re-flashing the same build is the usual case; a longer image sharing
the old one's leading bytes is the same case. Anything else mismatches.

A surviving record is the normal outcome on CH591/CH592, where the record lives
in DataFlash and a code-flash erase does not reach it (`minichlink -E` does not
touch DataFlash). On CH570/CH572 the record shares code flash, so a whole-chip
erase takes it with the application and the device lands in the bootloader.

### 9.2 RAM boot request (app → bootloader entry)

An application asks to re-enter the bootloader by writing
`OB_BOOTREQ_MAGIC = 0xB007CA11` to a reserved top-of-RAM word and
performing a software reset. The bootloader checks the word during its boot
decision and **always clears it** (a request fires once).

| Family | Address | RAM |
|---|---|---|
| CH570 / CH572 | `OB_BOOTREQ_ADDR_CH57X = 0x20002FF0` | 12 KiB |
| CH591 / CH592 | `OB_BOOTREQ_ADDR_CH59X = 0x200067F0` | 26 KiB |

The **top 16 bytes of RAM are reserved** for this word: the bootloader's
linker script places the stack below them (`_eusrstack =
ORIGIN + LENGTH − 16`) so early stack pushes cannot clobber the magic, and
applications integrating with OpenBoot must reserve the same 16 bytes (see
`firmware/app/`).

## 10. Boot decision and idle auto-boot

At reset the bootloader decides, in this exact order:

1. Board boot strap asserted → stay in OpenBoot.
2. RAM boot-request magic present → stay in OpenBoot.
3. Boot record invalid → stay in OpenBoot.
4. First app word is the erased pattern (`0xF3F9BDA9`) → stay in OpenBoot.
5. Otherwise → jump to `0x2000`.

The request word is always cleared after it is read, including when the strap
wins, so it is one-shot. No shipped board defines the optional OpenBoot strap;
products may set `OB_BOOT_PIN_MASK`. It is sampled only at reset.

With the build-time `OB_BOOT_IMAGE_CRC=1` setting, the device also checks the
full image against the record before jumping. This is off by default because
of its startup cost; record integrity is always checked.

Be precise about what that buys. It proves the app region still holds the bytes
COMMIT attested, which catches corruption and catches an out-of-band reflash
that diverges from a surviving record (section 9.1). It is **not** an identity
check: by the prefix property it also passes an image that merely begins with
the recorded one, and nothing in the record names a product, a version, or a
build. A device that must refuse foreign images needs a check above OBP —
COMMIT attests a length and a CRC by design, so any conforming host can flash
anything that fits the app region.

**Idle auto-boot:** a device that stays in the bootloader with a *valid*
boot record starts a poll-iteration counter. Its limit is derived from the
board's `OB_IDLE_TIMEOUT_MS` setting (default `10000`). If the counter expires
while still in IDLE — no successful HELLO this power cycle — the device boots
the app, so a device that entered the bootloader by strap glitch or stray boot
request recovers by itself.

A held strap does not inhibit idle auto-boot because it is not sampled again;
it provides a connection window, not permanent residency.

The first successful HELLO disables auto-boot until the next reset;
**no traffic of any kind resets the timer** — valid, rejected, malformed and
CRC-corrupt frames alike all advance it, so nothing short of a session can
keep a device out of its app indefinitely.

The timer counts poll iterations rather than wall-clock time. Frame handling
can stretch the interval, so treat `OB_IDLE_TIMEOUT_MS` as a lower bound. A
device without a valid record waits indefinitely.

## 11. Versioning and discovery

- The version is `major.minor`, exchanged in both HELLO directions
  (`OB_PROTO_MAJOR = 0x00`, `OB_PROTO_MINOR = 0x01`).
- **Pre-1.0 (major 0), nothing is frozen**: any minor bump may change
  behavior incompatibly, so a major-0 device requires an EXACT
  major+minor match in HELLO and rejects anything else with `E_PROTO`.
  The additive-minor rules below take effect at 1.0.
- **Major** changes are incompatible. A device rejects an unsupported
  `host_major` with `E_PROTO`; a host MUST refuse to proceed if the
  device's `proto_major` is not one it implements.
- **Minor** changes are additive only, in exactly two ways:
  1. **Response payload growth.** Later minors may append fields to
     response payloads (HELLO in particular). A device MUST send at least
     the lengths specified here; a host MUST accept longer payloads and
     ignore bytes it does not understand. A host MUST NOT reject a HELLO
     response for being longer than 36 bytes.
  2. **New opcodes / gated features**, discovered by probing: an
     unimplemented opcode is answered with `E_CMD`, which is always safe
     and side-effect-free. Feature bits in HELLO (`features`) advertise
     optional capabilities such as READ without a round trip.
- **`flags` is the escape hatch**: it MUST be zero in v0.1 and v0.1 devices
  reject nonzero flags with `E_ARG`, so a future host can detect — from the
  rejection — that a device does not implement a flag-gated behavior.

Host retry/timeout guidance (matches the reference `openboot` tool):

| Exchange | Timeout |
|---|---|
| HELLO | 500 ms |
| WRITE | 1 s |
| ERASE | 200 ms + 30 ms per 4 KiB block |
| CRC / COMMIT | 3 s |

At most 3 attempts per request, each with a fresh `seq`; responses with a
stale `seq` are discarded. COMMIT exact-replay behavior is defined in
section 6.5. **BOOT is never retried** (section 6.6).

## 12. USB identity

| Item | Value |
|---|---|
| VID:PID | `0x1209:0x0001` — the pid.codes **test PID**. Interim only: a permanent pid.codes allocation will be filed when the repository is public, and the PID will change. Hosts should select on VID:PID *plus* serial/HELLO identity, not PID alone. |
| Class | HID, one interface, one configuration |
| Report descriptor | vendor usage page `0xFF00`; 64-byte input and output reports; **no report IDs** |
| Endpoints | EP1 IN + EP1 OUT, interrupt, 64 bytes |
| `iSerial` | the 64-bit ROM UID as 16 hex digits, most-significant nibble first (`printf "%016X"` of the u64 reported in HELLO) — used by the host `--serial` selector |
| `bcdDevice` | the bootloader version (= HELLO `bl_version`) |

## 13. UART parameters

| Item | Value |
|---|---|
| Line | 115200 baud, 8 data bits, no parity, 1 stop bit |
| Flow control | none |
| Discovery | none — the host is told the port (`--port`); there is no auto-scan and no probe broadcast |
| Framing | section 4.2 |

## 14. Golden frames

Normative examples live in [`protocol/golden_frames.txt`](../protocol/golden_frames.txt).
Regenerate them with `python3 protocol/gen_protocol.py`. They contain logical
frames: prepend `B0 07` for UART, or zero-pad to a 64-byte USB report.
