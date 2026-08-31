"""USB-transport bench helpers: ab_bench's structure with the OBP client
over HID (64-byte zero-padded reports; no SOF — see proto/mod.rs encode) and
CLI calls on the default usb transport.

Runnable: `usb_bench.py [scenario ...]` (default: all). The USB regime is
what makes negative_verify evidentiary here — see that scenario's docstring.

Fixture lessons this file encodes (nanoCH592, WCH-LinkE, board USB-powered):
 - `minichlink -b` only resets reliably from a HALTED chip: -A first.
 - This board's SRAM decays in <50 ms, so an armed boot request never
   survives a power cut; re-enter the bootloader by letting the app arm the
   request, then -A + -b (soft reset, SRAM intact).
 - A board powered from its own USB cable CANNOT be power-cut by the LinkE
   (-t is a no-op); mid-op power-cut tests need LinkE-only power with a
   UART transport, or another board.
 - minichlink -w does not write RAM on ch5xx; recovery from a slot full of
   committed garbage is kill_record() + booting the witness in the OTHER
   slot.
 - SWD reads (read_region) halt the core and outlast the 10 s idle window:
   always re-enter the bootloader afterwards."""
import os, random, re, sys, time, zlib
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import ab_bench as ab
import hid

ROOT = ab.ROOT
CFG = dict(
    serial="CEBD8F0653EF",   # adjust per fixture
    boot=f"{ROOT}/firmware/build/generic-ch592-usb/openboot-generic-ch592-usb.bin",
    bootreq=0x200067F0,
)
CFG["port"] = None   # no UART on this fixture

class UsbObp:
    """ab.Obp over HID: same frame bytes inside one 64-byte report."""
    def __init__(self, tries=20):
        self.d = None
        for _ in range(tries):           # ride out re-enumeration
            try:
                self.d = hid.device()
                self.d.open(0x1209, 0x0001)
                break
            except OSError:
                self.d = None
                time.sleep(0.5)
        if self.d is None:
            raise RuntimeError("bootloader HID device never enumerated")
        self.seq = 0

    def send_raw(self, cmd, payload=b""):
        """Fire-and-forget: one report, no response read. USB frames carry
        no SOF (proto/mod.rs encode): header + payload + CRC32, zero-padded
        into one 64-byte report behind hidapi's report-ID byte."""
        self.seq = (self.seq + 1) & 0xFF
        body = bytes([cmd, self.seq, len(payload), 0]) + payload
        f = body + zlib.crc32(body).to_bytes(4, "little")
        self.d.write(bytes([0]) + f + bytes(64 - len(f)))

    def xfer(self, cmd, payload=b""):
        """Only a VALIDATED response satisfies the exchange: correct
        command echo, this sequence number, a sane length and a good CRC.
        Anything else (a stale queued report, line corruption) is dropped
        and the read continues — a leftover E_FLASH from an earlier
        exchange must never be creditable to the current one."""
        self.send_raw(cmd, payload)
        deadline = time.time() + 2.0
        while time.time() < deadline:
            r = bytes(self.d.read(64, timeout_ms=200))
            if len(r) < 8:
                continue
            n = r[2]
            if r[0] != (cmd | 0x80) or r[1] != self.seq or 4 + n + 4 > len(r):
                continue
            body, crc = r[:4 + n], r[4 + n:4 + n + 4]
            if zlib.crc32(body).to_bytes(4, "little") != crc:
                continue
            return body + crc
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

    def crc(self, addr, length):
        r = self.xfer(0x04, addr.to_bytes(4, "little")
                      + length.to_bytes(4, "little"))
        if self.status(r) != 0:
            raise RuntimeError(f"CRC status {self.status(r)}")
        return int.from_bytes(r[5:9], "little")

    def close(self):
        if self.d:
            self.d.close()


def cli(*args, t=300):
    return ab.run([ab.OB, *args], t)

def probe():
    r = cli("probe", t=30)
    act = wr = base = cap = uidhex = None
    for line in r.stdout.splitlines():
        if "slots" in line:
            act = line.split("active")[1].split(",")[0].strip()
            wr = line.split("writing")[1].strip().rstrip(")")
        if "write window" in line:
            p = line.split()[2].split("..")
            base, cap = int(p[0], 16), int(p[1], 16) - int(p[0], 16)
        if "uid" in line:
            uidhex = line.split()[-1]
    return act, wr, base, cap, uidhex

def factory(image):
    ab.factory(dict(CFG, boot=image))

def swap_boot(image):
    import tempfile
    boot = open(image, "rb").read()
    ab.mc(CFG, "-w", image, "0x0")
    with tempfile.NamedTemporaryFile(suffix=".bin") as rb:
        ab.mc(CFG, "-r", rb.name, "0x0", str(len(boot)))
        if open(rb.name, "rb").read() != boot:
            raise RuntimeError("swap_boot readback mismatch")
    ab.power_cycle(CFG)

def read_region(addr, length):
    import tempfile
    ab.mc(CFG, "-A", t=30)
    with tempfile.NamedTemporaryFile(suffix=".bin") as rb:
        ab.mc(CFG, "-r", rb.name, hex(addr), str(length), t=300)
        return open(rb.name, "rb").read()

def enter_bootloader(tries=4):
    """This board's SRAM decays within 50 ms, so an armed boot request never
    survives a power cut — the committed app boots straight away. But the
    witness app arms the request immediately, so: let the app run (power
    cycle), then SOFT reset (-b, SRAM intact) and the boot decision keeps
    control."""
    import time as _t
    for _ in range(tries):
        r = probe()
        if r[0]:
            return r
        ab.power_cycle(CFG)           # app runs and arms the request
        _t.sleep(1.0)
        ab.mc(CFG, "-A", t=30)        # -b only resets reliably from halt
        ab.mc(CFG, "-b", t=30)        # soft reset: request survives
        _t.sleep(1.2)
        r = probe()
        if r[0]:
            return r
    return (None,) * 5


def kill_record(slot_base, cap=0x36000):
    """Invalidate one slot's boot record over SWD (whole-sector effect of
    minichlink's write is fine: the record block holds only the record)."""
    import tempfile, os as _os
    ab.mc(CFG, "-A", t=30)
    with tempfile.NamedTemporaryFile(suffix=".bin", delete=False) as f:
        f.write(b"\x00" * 4); pth = f.name
    ab.mc(CFG, "-w", pth, hex(slot_base + cap), t=60)
    _os.unlink(pth)

E_FLASH = 0x08
DETAIL_VERIFY_MISMATCH = 0x72       # flash_ch5xx.h OB_FLERR_VERIFY_MISMATCH


def scenario_negative_verify(open_build=True):
    """Verify honesty at the USB build's 60 MHz flash timing: PRNG pattern
    B over PRNG pattern A without an erase must come back E_FLASH with the
    driver's verify-mismatch detail — the capture that proves verify
    detects inequality on silicon. Random-over-random, never the 0xF0->
    0x0F nibble swap: see flash_driver_bench.scenario_negative_verify for
    the marginal-cell evidence (383/384 bits of that swap landed at 60 MHz
    once, the holdout reading 0x0F via the controller but 0x07 over SWD).
    The contract stays dual-outcome: the wire must match SWD either way."""
    print("\n=== usb: verify report must match SWD ground truth ===")
    act, wr, base, cap, _ = enter_bootloader()
    if base is None:
        raise RuntimeError("bootloader did not answer")
    rnd = random.Random(0x0E05)
    pat = bytes(rnd.getrandbits(8) for _ in range(48))
    corrupt = bytes(rnd.getrandbits(8) for _ in range(48))
    c = UsbObp()
    try:
        c.hello()
        ab.check("erase", c.status(c.erase(base, 4096)), 0)
        ab.check("program PRNG A x48 (driver-verified)",
                 c.status(c.write(base, pat)), 0)
        r = c.write(base, corrupt)
        st = c.status(r)
    finally:
        c.close()
    got = read_region(base, 48)      # halts the core: re-enter afterwards
    if st is None:
        # A lost response is a transport/bench failure, not a driver
        # refusal — report it as such rather than crash indexing r.
        ab.check("a response arrived for the overprogram attempt", None, 0)
    elif st == 0:
        ab.check("wire says success and flash really holds the new bytes",
                 got == corrupt, True)
    else:
        ab.check("wire says E_FLASH", st, E_FLASH)
        if open_build:
            ab.check("detail is the driver's verify-mismatch code",
                     r[5], DETAIL_VERIFY_MISMATCH)
        ab.check("and flash really does NOT hold the requested bytes",
                 got != corrupt, True)
    print(f"  outcome: status={st} "
          f"flash={'landed' if got == corrupt else 'refused'}")


def scenario_soak():
    """Bulk fill at the USB build's 60 MHz flash timing: 16 KiB of PRNG
    through the open driver's raw erase/write, live CRC + SWD readback as
    driver-independent ground truth, uid stable across the SWD halt."""
    print("\n=== usb: 60 MHz soak, SWD readback ===")
    act, wr, base, cap, uid1 = enter_bootloader()
    if base is None:
        raise RuntimeError("bootloader did not answer")
    rnd = random.Random(0x60E5)
    img = bytes(rnd.getrandbits(8) for _ in range(16384))
    want = zlib.crc32(img) & 0xFFFFFFFF
    obp_fill(base, img)
    c = UsbObp()
    try:
        c.hello()
        ab.check("live CRC matches the local image", c.crc(base, len(img)), want)
    finally:
        c.close()
    got = read_region(base, len(img))
    ab.check("SWD readback matches byte-for-byte", got == img, True)
    r = enter_bootloader()
    ab.check("uid stable across the SWD halt", r[4], uid1)


def scenario_marginal_overprogram(tries=64):
    """Hunt the marginal-overprogram refusal: the 0xF0->0x0F nibble swap
    the 2026-08-27 bench caught failing at 60 MHz (E_FLASH detail 0x72),
    and which landed 383/384 bits in one attempt today — this die's
    program op can usually rewrite arbitrary values, so mismatches only
    come from marginal program pulses. Each attempt uses a fresh 256 B
    page; the loop stops at the first refusal, which is the transcript
    proving the driver's verify path reports a real mismatch on silicon
    (dual-outcome as ever: the wire must match SWD either way). A die
    that absorbs every attempt leaves mismatch REPORTING pinned by the
    host register-mock tests; that outcome is recorded, not failed."""
    print(f"\n=== usb: marginal-overprogram hunt (0xF0->0x0F, {tries} pages) ===")
    act, wr, base, cap, _ = enter_bootloader()
    if base is None:
        raise RuntimeError("bootloader did not answer")
    pat, corrupt = bytes([0xF0]) * 48, bytes([0x0F]) * 48
    span = (tries * 256 + 4095) & ~4095
    c = UsbObp()
    refusal = None
    try:
        c.hello()
        ab.check("erase", c.status(c.erase(base, span)), 0)
        for i in range(tries):
            a = base + i * 256
            if c.status(c.write(a, pat)) != 0:
                raise RuntimeError(f"setup program failed at {a:#x}")
            r = c.write(a, corrupt)
            st = c.status(r)
            if st is None:
                # Lost response: a bench/transport fault, not a refusal.
                raise RuntimeError(f"no response to the overprogram at {a:#x}")
            if st != 0:
                refusal = (i, a, st, r[5])
                break
    finally:
        c.close()
    if refusal:
        i, a, st, detail = refusal
        got = read_region(a, 48)
        print(f"  refusal on attempt {i} at {a:#x}: "
              f"flash reads {got.hex(' ')}")
        ab.check("wire says E_FLASH", st, E_FLASH)
        ab.check("detail is the driver's verify-mismatch code",
                 detail, DETAIL_VERIFY_MISMATCH)
        ab.check("and flash really does NOT hold the requested bytes",
                 got != corrupt, True)
    else:
        got = read_region(base + (tries - 1) * 256, 48)
        ab.check("last attempt: wire success backed by SWD",
                 got == corrupt, True)
        print(f"  die absorbed all {tries} overprograms; verify-mismatch "
              "reporting rests on the host register-mock tests")


SCENARIOS = {
    "soak": scenario_soak,
    "negative_verify": scenario_negative_verify,
    "marginal_overprogram": scenario_marginal_overprogram,
}


def main():
    picks = sys.argv[1:] or list(SCENARIOS)
    ab.fails = []
    for p in picks:
        SCENARIOS[p]()
    print("\n>>> usb: " + (f"{len(ab.fails)} FAILURES: {ab.fails}"
                           if ab.fails else "ALL CHECKS PASSED"))
    return 1 if ab.fails else 0


def obp_fill(base, data):
    """Raw erase + write of `data` at base through the running bootloader's
    flash driver — no COMMIT, so the active app and records are untouched."""
    c = UsbObp()
    try:
        c.hello()
        n = (len(data) + 4095) & ~4095
        st = c.status(c.erase(base, n))
        if st != 0:
            raise RuntimeError(f"erase status {st}")
        for off in range(0, len(data), 48):
            chunk = data[off:off + 48]
            if len(chunk) % 4:
                chunk = chunk + b"\xFF" * (4 - len(chunk) % 4)
            st = c.status(c.write(base + off, chunk))
            if st != 0:
                raise RuntimeError(f"write status {st} at {base+off:#x}")
    finally:
        c.close()


if __name__ == "__main__":
    raise SystemExit(main())
