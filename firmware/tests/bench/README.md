# A/B hardware bench

The host suite in `../` proves the A/B logic against a simulated flash. It
cannot prove the simulation matches silicon. This directory holds what was
actually run on parts, so the claim "an interrupted update leaves the previous
application bootable" rests on hardware rather than on a model.

**This is bench-specific.** Probe serials, serial ports and board files in
`ab_bench.py` describe one desk; change `CHIPS` to match yours. It is not run
by `make test` and needs a WCH-LinkE per part.

```sh
./build_witness.sh 0x2000  0x20002000 0xAAAA0001 0x20002100 0x20002FF0 ch572-A.bin
./build_witness.sh 0x1F000 0x20002100 0xBBBB0002 0x20002000 0x20002FF0 ch572-B.bin
python3 -c "import ab_bench; ab_bench.run_all('ch572')"
```

## What it checks

`scenario_lifecycle` — a fresh part offers slot A and reports nothing active;
after committing A the target moves to B and the write base moves with it; an
image based at the *active* slot is refused before anything is erased; after
committing B the target returns to A. Both records end valid with B
outranking A.

`scenario_interrupted` — the acceptance test. Begins an update into the
inactive slot over a minimal in-line OBP client, cuts power part-way (after
ERASE, after one write, and after the whole image but before COMMIT), and
requires the part to come back up running the PREVIOUS application unaided.

`scenario_recovery` — a retry after an interrupted update completes normally
and takes over.

## Why the evidence looks the way it does

Every post-cut observation is made over SWD. **Opening or closing the
WCH-Link CDC port resets the target**, so a UART check perturbs the thing it
is measuring: the bootloader answers, the tool exits, the port closes, and the
part promptly resets and boots the app.

The witness chain avoids that entirely:

- **bootreq word** — only an application writes `0xB007CA11`, and the boot
  decision clears it whenever the bootloader keeps control. Armed means an
  application has run since the bootloader last did.
- **witness words** — each image zeroes the other slot's word before writing
  its own constant, so exactly one is ever set and it names the image that
  ran. Plain constants, not counters: SRAM here *decays* across a power cut
  rather than clearing or surviving, so a counter compared across a cut is
  noise, while a specific 32-bit value cannot be fabricated by decay.
- **records read from flash** — which slots are valid, and their generations.

The witness images arm the boot request because SWD RAM writes do not stick
through minichlink, so it is the only way back into the bootloader once an app
is installed. That makes resets alternate app/bootloader, which is why
`enter_bootloader()` power-cycles in a loop.

Two consequences of the reset-on-close behaviour are worth knowing before
changing any of this:

- The acceptance test cuts power with the serial port still **open**, and
  reads the outcome over SWD before closing it. Closing first would reset the
  part, run the app and re-arm the magic *before* the cut, which would leave
  the check dependent on whether that word happened to decay — an intermittent
  failure that has nothing to do with the firmware.
- `app_ran()` is only meaningful straight after a power cut, for the same
  reason. The lifecycle scenario therefore uses the witness words, which
  nothing consumes.
