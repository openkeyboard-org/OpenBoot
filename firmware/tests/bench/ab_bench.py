#!/usr/bin/env python3
"""A/B hardware bench for OpenBoot 3a+3b.

Drives a real part through the slot lifecycle and the interrupted-update
acceptance test. Uses the openboot CLI for the normal flows and a minimal
in-line OBP client for the interrupted one, where the point is to stop at a
chosen instant rather than to complete.
"""
import os, re, subprocess, sys, time, zlib

ROOT = os.environ.get("OPENBOOT_ROOT", os.path.abspath(os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "..", "..")))
MC = os.path.expanduser("~/Development/Personal/WCH/ch32fun/minichlink/minichlink")
OB = f"{ROOT}/tools/target/release/openboot"
HERE = os.path.dirname(os.path.abspath(__file__))

CHIPS = {
    "ch572": dict(serial="C2228F064754", port="/dev/ttyACM2",
                  boot=f"{ROOT}/firmware/build/ch572-uart/openboot-ch572-uart.bin",
                  slot_b=0x1F000, cap=0x1C000, bootreq=0x20002FF0),
    "ch592": dict(serial="CEBD8F0653EF", port="/dev/ttyACM0",
                  boot=f"{ROOT}/firmware/build/ch592-uart+bench-ch592/openboot-ch592-uart.bin",
                  slot_b=0x39000, cap=0x36000, bootreq=0x200067F0),
}
fails = []

def run(cmd, t=90):
    return subprocess.run(cmd, capture_output=True, text=True, timeout=t)

def mc(cfg, *args, t=90, check_status=True):
    """Run minichlink against one probe, action FIRST.

    Ordering is not cosmetic. minichlink derives skip_startup from argv[1]
    alone, so `-l <serial> -A` runs SetupInterface where `-A -l <serial>` does
    not - and LESetupInterface asserts ndmreset (pgm-wch-linke.c), i.e. it
    RESETS the target. For -A and -t that is precisely what the flag is asking
    to avoid, and this file's own README documents the action-first form
    (firmware/README.md: `minichlink -kt -l <probe-serial>`).

    Scope, so this is not oversold: -E/-w/-r are absent from argv[1]'s skip
    list in EITHER ordering, so they always run SetupInterface and this change
    does not make a read non-perturbing. It restores the intended behaviour for
    -A and -t only.

    -l trails the action's own operands: -w takes <file> <addr> and -r takes
    <file> <addr> <len> positionally, so inserting "-l <serial>" between them
    would hand "-l" to -w as a filename.

    check_status: minichlink's exit status was previously discarded here, which
    is how a power cut that never happened could still let a scenario report
    PASS - the worst shape of failure for an acceptance test. A zero exit is
    also not sufficient on its own: minichlink returns 0 on paths where it
    never reached the chip, so the known "no target" strings are treated as
    failures too.
    """
    if not args:
        # Would put an option at argv[1] and break the very invariant this
        # function exists to hold. No caller does it; fail rather than emit a
        # command that quietly disables skip_startup.
        raise ValueError("mc() needs a minichlink action; see the docstring")
    action, rest = args[0], list(args[1:])
    if action in ("-t", "-3"):
        # Power control must not need a live chip: -k skips programmer init so
        # the rail can be cut and restored on a part that is not responding,
        # which is the state a power-cut test creates.
        action = "-k" + action.lstrip("-")
    cmd = [MC, action, *rest, "-l", cfg["serial"]]
    r = run(cmd, t)
    if check_status:
        blob = f"{r.stdout}\n{r.stderr}"
        if r.returncode != 0:
            raise RuntimeError(
                f"{' '.join(cmd)} -> exit {r.returncode}\n{blob.strip()}")
        for marker in ("Could not setup interface", "Chip Type unknown",
                       "link error", "marchid : ffffffff"):
            if marker in blob:
                raise RuntimeError(
                    f"{' '.join(cmd)} exited 0 but never reached the chip "
                    f"({marker!r})\n{blob.strip()}")
    return r

def check(label, got, want):
    ok = got == want
    print(f"  [{'PASS' if ok else 'FAIL'}] {label}: {got}" + ("" if ok else f"  (expected {want})"))
    if not ok:
        fails.append(label)
    return ok

def factory(cfg):
    """Whole-chip erase + bootloader, so every scenario starts identical.

    The write is read back and compared. minichlink ignores the return values
    of its CH5xx erase/write calls, so a zero exit status is NOT evidence the
    image landed (the firmware Makefile's flash target documents the same
    rule) - and a factory() that silently did nothing once cost a debugging
    session that chased HELLO timeouts on a chip with no bootloader in it."""
    import tempfile
    boot = open(cfg["boot"], "rb").read()
    mc(cfg, "-E")
    mc(cfg, "-w", cfg["boot"], "0x0")
    with tempfile.NamedTemporaryFile(suffix=".bin") as rb:
        mc(cfg, "-r", rb.name, "0x0", str(len(boot)))
        got = open(rb.name, "rb").read()
    if got != boot:
        raise RuntimeError(
            f"factory(): bootloader readback mismatch ({len(got)} B read); "
            "minichlink reported success for a write that did not land")
    power_cycle(cfg)

def reboot(cfg):
    """Always a real power cut. minichlink's -b ("reboot out of halt") does
    not re-run the boot decision here - the part carried on in the app and
    the bootloader never answered - so it is not usable as a reset."""
    power_cycle(cfg)

def read_word(cfg, addr, n=4):
    """Little-endian word at addr. minichlink prints '<addr>: xx xx ...' for
    any address, so match on the hex prefix rather than assuming RAM."""
    r = mc(cfg, "-r", "+", hex(addr), str(n), t=30)
    want = f"{addr:08x}"
    for line in r.stdout.splitlines():
        m = re.match(r"\s*([0-9a-fA-F]{8})\s*:\s*((?:[0-9a-fA-F]{2}\s*)+)", line)
        if m and m.group(1).lower() == want:
            b = bytes(int(x, 16) for x in m.group(2).split())
            return int.from_bytes(b[:4], "little")
    return None

def probe(cfg):
    """(active, write, base, capacity) from the CLI. Uses no debug probe, so
    it never perturbs what the part is doing: an answer means the BOOTLOADER
    is running, a timeout means an application is."""
    r = run([OB, "--transport", "uart", "--port", cfg["port"], "probe"], 30)
    act = wr = base = cap = None
    for line in r.stdout.splitlines():
        if "slots" in line:
            act = line.split("active")[1].split(",")[0].strip()
            wr = line.split("writing")[1].strip().rstrip(")")
        if "write window" in line:
            parts = line.split()[2].split("..")
            base, cap = int(parts[0], 16), int(parts[1], 16) - int(parts[0], 16)
    return act, wr, base, cap


def flash(cfg, image, base=None, extra=()):
    """A flat .bin carries no link base, so the slot-B image needs --base.
    That is precisely the gap the 3c bundle is meant to close."""
    cmd = [OB, "--transport", "uart", "--port", cfg["port"], "flash", image, "--force"]
    if base is not None:
        cmd += ["--base", hex(base)]
    return run(cmd + list(extra), 120)


# --- minimal OBP client, so an update can be stopped at a chosen instant ---
import serial

def frame(cmd, seq, payload=b""):
    body = bytes([cmd, seq, len(payload), 0]) + payload
    return b"\xB0\x07" + body + zlib.crc32(body).to_bytes(4, "little")


class Obp:
    def __init__(self, port):
        self.s = serial.Serial(port, 115200, timeout=1.5)
        self.seq = 0

    def xfer(self, cmd, payload=b""):
        """Responses carry the 0xB0 0x07 SOF too (uart_transport.c tr_send),
        so hunt for it rather than assuming the frame starts at byte 0."""
        self.seq = (self.seq + 1) & 0xFF
        self.s.reset_input_buffer()
        self.s.write(frame(cmd, self.seq, payload))
        deadline, win = time.time() + 2.0, b""
        while time.time() < deadline:
            b = self.s.read(1)
            if not b:
                continue
            win = (win + b)[-2:]
            if win == b"\xB0\x07":
                hdr = self.s.read(4)
                if len(hdr) < 4:
                    return None
                return bytes(hdr) + bytes(self.s.read(hdr[2] + 4))
        return None

    def status(self, r):
        return None if not r or len(r) < 5 else r[4]

    def hello(self):
        r = self.xfer(0x01, b"OBP1" + bytes([0, 2]))
        if self.status(r) != 0:
            raise RuntimeError(f"HELLO status {self.status(r)}")
        return r[4:]

    def erase(self, addr, length):
        return self.xfer(0x02, addr.to_bytes(4, "little") + length.to_bytes(4, "little"))

    def write(self, addr, data):
        return self.xfer(0x03, addr.to_bytes(4, "little") + data)

    def close(self):
        self.s.close()


def power_cycle(cfg, off=0.6, settle=2.0):
    """A real power cut - the parts are probe-powered."""
    mc(cfg, "-t", t=30)
    time.sleep(off)
    mc(cfg, "-3", t=30)
    time.sleep(settle)


MARK_A, MARK_B = (0x20002000, 0xAAAA0001), (0x20002100, 0xBBBB0002)
STK_MARK = 0xC5000000   # witness word at MARK+8: 0xC5000000 | SysTick SR at entry


def app_ran(cfg):
    """True when an application has run since the bootloader last had control.

    Only meaningful straight after a POWER CUT. The magic does not survive a
    cut intact (SRAM decays), so the boot decision sees no request and hands
    control to the application, which re-arms it. After a reset with SRAM
    intact - which is what closing the CDC port causes - the bootloader finds
    the magic still armed, keeps control and clears it, and this reads False
    however the part actually behaved.

    Read over SWD, never over UART: only an application writes the boot-request
    magic, and ob_boot_decide() clears it whenever the bootloader keeps
    control. The obvious UART version - "does the bootloader answer a probe?" -
    is unsound here, because opening or closing the WCH-Link CDC port resets
    the target, so asking the question changes the answer."""
    mc(cfg, "-A", t=30)
    return read_word(cfg, cfg["bootreq"]) == MAGIC


def witness(cfg):
    """Which marker constants are present. Only meaningful for a run since
    the last power cut, which is how every caller uses it: SRAM decays across
    a cut, so a stale constant does not survive intact, and decay cannot
    manufacture one either."""
    mc(cfg, "-A", t=30)
    return (read_word(cfg, MARK_A[0]) == MARK_A[1],
            read_word(cfg, MARK_B[0]) == MARK_B[1])


def slot_records(cfg):
    """(slot A record magic+gen, slot B record magic+gen) straight from flash."""
    mc(cfg, "-A", t=30)
    out = []
    for base in (0x2000, cfg["slot_b"]):
        rec = base + cfg["cap"]
        out.append((read_word(cfg, rec), read_word(cfg, rec + 4)))
    return out


OBR2 = 0x3252424F


def scenario_lifecycle(name):
    cfg = CHIPS[name]
    print(f"\n=== {name}: A/B slot lifecycle ===")
    factory(cfg)

    act, wr, base, cap = probe(cfg)
    check("fresh part: nothing active, slot A is the target", (act, wr), ("none", "A"))
    check("fresh write window", (hex(base), hex(cap)), ("0x2000", hex(cfg["cap"])))

    check("flash into slot A succeeds", flash(cfg, f"{name}-A.bin").returncode, 0)
    time.sleep(1.0)
    # Evidence here is the witness words alone, not app_ran(): the CLI closes
    # the CDC port on exit, which resets the target, so the bootloader has
    # already consumed the boot-request magic by the time it could be read.
    # The witness words are consumed by nothing and say which image ran.
    check("slot A's image ran", witness(cfg), (True, False))
    # Exactly STK_MARK means the bootloader handed over a clean SysTick
    # CNTIF (#18); low bits would carry a stale flag.
    check("handoff SysTick flag clean (A)", read_word(cfg, MARK_A[0] + 8), STK_MARK)

    reboot(cfg)
    act, wr, base, _ = probe(cfg)
    check("committing A makes B the target", (act, wr), ("A", "B"))
    check("write base moved to slot B", hex(base), hex(cfg["slot_b"]))

    r = flash(cfg, f"{name}-A.bin")          # declares base 0x2000, i.e. slot A
    check("an image based at the ACTIVE slot is refused",
          r.returncode != 0 and "write base" in (r.stdout + r.stderr), True)
    check("  ...and that attempt erased nothing", probe(cfg)[:2], ("A", "B"))

    check("flash into slot B succeeds",
          flash(cfg, f"{name}-B.bin", base=cfg["slot_b"]).returncode, 0)
    time.sleep(1.0)
    check("slot B's image ran", witness(cfg), (False, True))
    check("handoff SysTick flag clean (B)", read_word(cfg, MARK_B[0] + 8), STK_MARK)

    reboot(cfg)
    act, wr, base, _ = probe(cfg)
    check("committing B makes A the target again", (act, wr), ("B", "A"))
    check("write base back at slot A", hex(base), "0x2000")
    ra, rb = slot_records(cfg)
    check("both records valid, B outranks A", (ra[0] == OBR2, rb[0] == OBR2, rb[1] > ra[1]),
          (True, True, True))


def enter_bootloader(cfg, tries=4):
    """Power-cycle until the BOOTLOADER answers. Resets alternate: the app
    re-arms the boot request every run and the boot decision consumes it, so
    one cycle is not always enough. Also clears any lingering debug halt."""
    for _ in range(tries):
        power_cycle(cfg)
        r = probe(cfg)
        if r[0]:
            return r
    return (None, None, None, None)


MAGIC = 0xB007CA11


def scenario_interrupted(name, writes, label):
    # writes=None means "the whole image": derived from the image length so
    # the label stays true as the witness grows. It already lied once - the
    # image is 42 bytes and the old hardcoded 2 writes covered 32 of them, so
    # the "whole image, before COMMIT" cut had never actually been run.
    """The acceptance test: begin an update into the inactive slot, cut power
    part-way, and require the device to come back up RUNNING the previous
    application with no host involvement.

    Every observation after the cut is made over SWD only. Opening or closing
    the WCH-Link CDC port RESETS the target, so any UART-based check would
    perturb the very thing being measured.

    The evidence is a chain that does not depend on timing:
      - bootreq armed: only an application writes the magic, and the
        bootloader CLEARS it whenever it keeps control, so an armed word
        means an application has run since the bootloader last ran;
      - witness B set and witness A cleared: each image clears the other's
        word, so this names the image that ran;
      - slot A's record invalid, slot B's intact: A could not have been
        chosen, and B is exactly as it was before the update began.
    """
    cfg = CHIPS[name]
    print(f"\n=== {name}: power cut {label} ===")
    act, wr, base, _ = enter_bootloader(cfg)
    if (act, wr) != ("B", "A"):
        print(f"  SKIP: need B active / A target, got {act}/{wr}")
        return
    img = open(f"{HERE}/{name}-A.bin", "rb").read()
    if writes is None:
        writes = (len(img) + 15) // 16

    c = Obp(cfg["port"])
    try:
        c.hello()
        check("ERASE accepted (also invalidates slot A's record)",
              c.status(c.erase(base, 4096)), 0)
        for i in range(writes):
            r = c.write(base + i * 16, img[i * 16:i * 16 + 16].ljust(16, b"\xFF"))
            # Setup, not a property under test: a failed write means the cut
            # lands in a different state than the label claims, so abort
            # rather than record a PASS/FAIL about the wrong scenario. A
            # raise, not an assert: asserts vanish under python -O, and the
            # try/finally guarantees the port closes on this path - left
            # open, it would wedge every later scenario in this process.
            if c.status(r) != 0:
                raise RuntimeError(f"setup WRITE {i} failed: status {c.status(r)}")

        # Cut with the port still OPEN, and read the outcome before closing
        # it. Closing resets the target, which would run the app and re-arm
        # the magic before the cut - leaving "was it armed afterwards?"
        # dependent on whether the word happened to decay, which is exactly
        # the flake this avoids. Going in, the bootloader has had control, so
        # the magic is clear; only an application can set it, so finding it
        # armed after the cut is proof.
        power_cycle(cfg)                  # the cut, before any COMMIT
        time.sleep(1.5)
        mc(cfg, "-A", t=30)
        bq = read_word(cfg, cfg["bootreq"])
        wa = read_word(cfg, MARK_A[0])
        wb = read_word(cfg, MARK_B[0])
        ra, rb = slot_records(cfg)

        check("an application ran unaided after the cut", bq, MAGIC)
        check("the image that ran is slot B's", (wa, wb), (0, MARK_B[1]))
        check("slot A's record is gone (it was mid-update)", ra[0] != OBR2, True)
        check("slot B's record survived intact", rb[0], OBR2)
        check("slot B still outranks", rb[1] >= 1, True)
    finally:
        c.close()


def scenario_recovery(name):
    """After an interrupted update the device must still take a retry: the
    half-written slot is re-erased, written and committed, and becomes the
    one that boots."""
    cfg = CHIPS[name]
    print(f"\n=== {name}: retry after the interrupted update ===")
    act, wr, base, _ = enter_bootloader(cfg)
    check("still offering the half-written slot as the target", (act, wr), ("B", "A"))
    check("retry flashes cleanly", flash(cfg, f"{name}-A.bin").returncode, 0)
    time.sleep(1.0)
    mc(cfg, "-A", t=30)
    check("the retried image is what runs now",
          (read_word(cfg, MARK_A[0]), read_word(cfg, MARK_B[0])), (MARK_A[1], 0))
    ra, rb = slot_records(cfg)
    check("slot A's record is valid again", ra[0], OBR2)
    check("and now outranks slot B", ra[1] > rb[1], True)
    check("A is active, B is the next target", enter_bootloader(cfg)[:2], ("A", "B"))


def run_all(name):
    global fails
    fails = []
    scenario_lifecycle(name)
    scenario_interrupted(name, 0, "after ERASE only")
    scenario_interrupted(name, 1, "after ERASE + 1 write")
    scenario_interrupted(name, None, "after ERASE + the whole image, before COMMIT")
    scenario_recovery(name)
    print(f"\n>>> {name}: " + (f"{len(fails)} FAILURES: {fails}" if fails else "ALL CHECKS PASSED"))
    return fails
