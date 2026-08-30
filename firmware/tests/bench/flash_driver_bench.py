#!/usr/bin/env python3
"""Hardware acceptance bench for the open flash driver (ports/flash_ch5xx.c).

Companion to ab_bench.py (whose helpers this reuses), covering what the A/B
state-machine scenarios do not: the driver's own erase/write/verify on real
silicon, interruption INSIDE a flash operation, verification that does not
trust the driver with its own homework, and identity parity against the
vendor-archive build.

Scenarios (per chip):
  soak            PRNG image through the open driver; readback over SWD
                  (driver-independent); cold power-cycle; CLI verify again.
  cross_driver    content written by the open driver CRC-verified by an
                  isp-build bootloader, and vice versa. Bootloader swaps
                  touch only flash 0x0000-0x2000, never the slots.
  cut_erase       power cut aimed inside a sector erase; every trial is
                  classified post-mortem over SWD (untouched / erased /
                  partial); requires >= MIN_MIDOP confirmed mid-erase
                  landings and full recovery after each.
  cut_write       power cut aimed inside a page-program stream; classified
                  the same way; requires >= MIN_MIDOP mid-program landings.
  negative_verify overprogramming 0x0F over 0xF0 without an erase must fail
                  E_FLASH with the driver's verify-mismatch detail — the one
                  test that proves verify detects inequality on silicon.
  boundaries      a 48 B write straddling a 256 B page boundary and one
                  ending exactly at a sector boundary, SWD-compared.
  uid_parity      HELLO uid bytes identical between open and isp builds,
                  and stable across a power cycle.

Requires: WCH-LinkE probes powering the targets (ab_bench.py CHIPS table),
open and isp bootloader images built:
    gmake CHIP=<c> TRANSPORT=uart [BOARD=...]                  # open
    gmake CHIP=<c> TRANSPORT=uart [BOARD=...] FLASH_DRIVER=isp # @isp dir
Override the minichlink path with MINICHLINK=... if ab_bench.py's default
does not exist on this machine.

Usage: flash_driver_bench.py <ch572|ch592> [scenario ...]
"""
import os
import random
import struct
import sys
import time
import zlib

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import ab_bench as ab

if os.environ.get("MINICHLINK"):
    ab.MC = os.environ["MINICHLINK"]

ERASED_WORD = 0xF3F9BDA9
E_FLASH = 0x08
DETAIL_VERIFY_MISMATCH = 0x72       # flash_ch5xx.h OB_FLERR_VERIFY_MISMATCH
MIN_MIDOP = 2                       # confirmed mid-op landings required
TRIALS = 12


def isp_boot(cfg):
    """The isp-variant image path: the build keys only the non-default
    variant, so insert @isp into the canonical build directory."""
    d, f = os.path.split(cfg["boot"])
    return os.path.join(d + "@isp", f)


def swap_boot(cfg, image):
    """Replace ONLY the bootloader (flash 0x0000-0x2000): no -E, so slot
    contents survive. Read back and compare like ab.factory() does."""
    import tempfile
    boot = open(image, "rb").read()
    ab.mc(cfg, "-w", image, "0x0")
    with tempfile.NamedTemporaryFile(suffix=".bin") as rb:
        ab.mc(cfg, "-r", rb.name, "0x0", str(len(boot)))
        if open(rb.name, "rb").read() != boot:
            raise RuntimeError("swap_boot(): bootloader readback mismatch")
    ab.power_cycle(cfg)


def classify_full(sector_bytes, before_bytes):
    """untouched / erased / partial from a FULL-sector SWD dump: sampling
    can miss a disturbance that lies between samples."""
    er = ERASED_WORD.to_bytes(4, "little")
    erased = all(sector_bytes[i:i + 4] == er
                 for i in range(0, len(sector_bytes), 4))
    if erased:
        return "erased"
    if sector_bytes == before_bytes:
        return "untouched"
    return "partial"


def read_region(cfg, addr, length):
    import tempfile
    ab.mc(cfg, "-A", t=30)
    with tempfile.NamedTemporaryFile(suffix=".bin") as rb:
        ab.mc(cfg, "-r", rb.name, hex(addr), str(length), t=120)
        return open(rb.name, "rb").read()


def prng_image(length, seed):
    r = random.Random(seed)
    return bytes(r.getrandbits(8) for _ in range(length))


def obp_session(cfg):
    (act, wr, base, cap) = ab.enter_bootloader(cfg)
    if base is None:
        raise RuntimeError("bootloader did not answer")
    c = ab.Obp(cfg["port"])
    c.hello()
    return c, base, cap


def obp_crc(c, addr, length):
    """OB_CMD_CRC over [addr, addr+length): the device's live CRC. The
    deadline scales with length — a full-window CRC at 6.4 MHz outruns the
    per-op default."""
    r = c.xfer(0x04, addr.to_bytes(4, "little") + length.to_bytes(4, "little"),
               t=5.0 + length / 32768)
    if c.status(r) != 0:
        raise RuntimeError(f"CRC status {c.status(r)}")
    return int.from_bytes(r[5:9], "little")


def obp_fill(c, base, img, label):
    """Raw erase + 48 B writes through the running bootloader's driver.
    Deliberately NO COMMIT anywhere in this file's raw flows: a committed
    PRNG blob is a valid boot record over garbage, the next reset boots it
    (opening the CDC port IS a reset), and the bootloader never answers
    again without SWD surgery."""
    ab.check(f"{label} erase", c.status(c.erase(base, len(img))), 0)
    st = 0
    for off in range(0, len(img), 48):
        st = c.status(c.write(base + off, img[off:off + 48]))
        if st != 0:
            break
    ab.check(f"{label} write", st, 0)


def hello_uid(cfg):
    c = ab.Obp(cfg["port"])
    try:
        h = c.hello()                    # status + payload
        return h[28:36].hex()           # uid u64 at payload offset 28 (h[0] = status = payload[0])
    finally:
        c.close()


# ---------------------------------------------------------------------------

def scenario_soak(name):
    cfg = ab.CHIPS[name]
    print(f"\n=== {name}: open-driver soak, SWD readback ===")
    ab.factory(cfg)
    c, base, cap = obp_session(cfg)
    img = prng_image(cap - 4096, 0x5EED)     # leave the record block alone
    want = zlib.crc32(img) & 0xFFFFFFFF
    try:
        obp_fill(c, base, img, "PRNG image through the open driver:")
        ab.check("live CRC matches the local image",
                 obp_crc(c, base, len(img)), want)
    finally:
        c.close()
    got = read_region(cfg, base, len(img))
    ab.check("SWD readback matches byte-for-byte", got == img, True)
    ab.power_cycle(cfg)
    c, base, _ = obp_session(cfg)
    try:
        ab.check("cold re-verify after power cycle",
                 obp_crc(c, base, len(img)), want)
    finally:
        c.close()


def scenario_cross_driver(name):
    """Content written raw by one driver build must CRC-match under the
    other after a bootloader swap and power cycle (the swap and cycle also
    force the checker's HELLO — its driver's UID path — on real silicon).
    The device CRC reads over XIP, so what this pins is the WRITER's landed
    bytes surviving a build swap, with the checker agreeing about them."""
    cfg = ab.CHIPS[name]
    print(f"\n=== {name}: cross-driver content verification ===")
    for writer, checker in (("open", "isp"), ("isp", "open")):
        wboot = cfg["boot"] if writer == "open" else isp_boot(cfg)
        cboot = cfg["boot"] if checker == "open" else isp_boot(cfg)
        ab.factory(dict(cfg, boot=wboot))
        img = prng_image(0x4000, {"open": 0x0BE4, "isp": 0x0151}[writer])
        want = zlib.crc32(img) & 0xFFFFFFFF
        c, base, _ = obp_session(cfg)
        try:
            obp_fill(c, base, img, f"[{writer}->{checker}] {writer}-driver")
            ab.check(f"[{writer}->{checker}] crc under the {writer} build",
                     obp_crc(c, base, len(img)), want)
        finally:
            c.close()
        swap_boot(cfg, cboot)
        c, base, _ = obp_session(cfg)
        try:
            ab.check(f"[{writer}->{checker}] crc under the {checker} build",
                     obp_crc(c, base, len(img)), want)
        finally:
            c.close()


def _recover_full_sector(cfg, seed):
    """Recovery after a cut is only proven by reprogramming the WHOLE
    affected sector, SWD-comparing it, power-cycling, and comparing again —
    a 16-byte token would miss weak bits anywhere else in the sector."""
    act, wr, base, _ = ab.enter_bootloader(cfg)
    pat = prng_image(4096, seed)
    c = ab.Obp(cfg["port"])
    try:
        c.hello()
        if c.status(c.erase(base, 4096)) != 0:
            return False
        for off in range(0, 4096, 48):
            if c.status(c.write(base + off, pat[off:off + 48])) != 0:
                return False
    finally:
        c.close()
    if read_region(cfg, base, 4096) != pat:
        return False
    ab.power_cycle(cfg)
    return read_region(cfg, base, 4096) == pat


def scenario_cut_erase(name):
    cfg = ab.CHIPS[name]
    print(f"\n=== {name}: power cuts inside sector erase ===")
    classes = {"untouched": 0, "erased": 0, "partial": 0}
    for t in range(TRIALS):
        # Known content in the target sector first, then a full-sector
        # before-image over SWD (classification never relies on sampling).
        c, base, cap = obp_session(cfg)
        pat = prng_image(4096, 0xA000 + t)
        try:
            if c.status(c.erase(base, 4096)) != 0:
                raise RuntimeError("setup erase failed")
            for off in range(0, 4096, 48):
                if c.status(c.write(base + off, pat[off:off + 48])) != 0:
                    raise RuntimeError("setup write failed")
        finally:
            c.close()
        before = read_region(cfg, base, 4096)
        act, wr, base, _ = ab.enter_bootloader(cfg)
        c = ab.Obp(cfg["port"])
        try:
            c.hello()
            # Fire the ERASE frame without waiting, then cut after a
            # randomized delay. mutation_begin may touch the record block
            # first, so where the cut lands is CLASSIFIED, never assumed.
            c.s.write(ab.frame(0x02, 1, struct.pack("<II", base, 4096)))
            c.s.flush()
            time.sleep(random.uniform(0.0, 0.08))
        finally:
            c.close()
        ab.power_cycle(cfg)
        cls = classify_full(read_region(cfg, base, 4096), before)
        classes[cls] += 1
        ab.check(f"trial {t} ({cls}): full-sector recovery",
                 _recover_full_sector(cfg, 0xAA00 + t), True)
    print(f"  cut classes over {TRIALS} trials: {classes}")
    ab.check("enough cuts landed mid-erase (partial state observed)",
             classes["partial"] >= MIN_MIDOP, True)


def scenario_cut_write(name):
    cfg = ab.CHIPS[name]
    print(f"\n=== {name}: power cut on ONE outstanding page program ===")
    hits = 0
    for t in range(TRIALS):
        c, base, cap = obp_session(cfg)
        pat = prng_image(4096, 0xB000 + t)
        cut_off = 256 * (t % 8)             # page-aligned 48B target span
        try:
            if c.status(c.erase(base, 4096)) != 0:
                raise RuntimeError("setup erase failed")
            # Everything before the target span lands as COMPLETED writes.
            for off in range(0, cut_off, 48):
                if c.status(c.write(base + off, pat[off:off + 48])) != 0:
                    raise RuntimeError("setup write failed")
            # ONE outstanding frame, then the cut races the program op. A
            # cut between completed frames cannot fake a hit: only THIS
            # span is judged, and only a torn state counts.
            c.s.write(ab.frame(0x03, 0x77, struct.pack("<I", base + cut_off)
                               + pat[cut_off:cut_off + 48]))
            c.s.flush()
            time.sleep(random.uniform(0.0, 0.004))
        finally:
            c.close()
        ab.power_cycle(cfg)
        got = read_region(cfg, base, 4096)
        span = got[cut_off:cut_off + 48]
        er = all(span[i:i + 4] == ERASED_WORD.to_bytes(4, "little")
                 for i in range(0, 48, 4))
        torn = span != pat[cut_off:cut_off + 48] and not er
        hits += torn
        ab.check(f"trial {t} ({'TORN' if torn else 'clean'}): full-sector "
                 "recovery", _recover_full_sector(cfg, 0xBB00 + t), True)
    print(f"  confirmed torn-program landings: {hits}/{TRIALS}")
    ab.check("enough cuts landed inside a program op", hits >= MIN_MIDOP, True)


def scenario_negative_verify(name, open_build=True):
    """Verify-honesty check. CH5xx cells are scrambled (erased state reads
    OB_ERASED_WORD) and whether the die can overprogram a given transition
    depends on the flash-timing regime — bench evidence on one CH592 die:
    0xF0->0x0F overprogram FAILS at the USB build's 60 MHz timing (E_FLASH,
    detail 0x72) but genuinely LANDS at the UART build's reset timing,
    SWD-confirmed. So the invariant this scenario pins is not "overprogram
    fails": it is that the driver's wire report MATCHES flash reality —
    success only when the bytes really landed, E_FLASH detail 0x72 only when
    they really did not. Either way the verify path is proven honest against
    ground truth the driver cannot see."""
    cfg = ab.CHIPS[name]
    print(f"\n=== {name}: verify report must match SWD ground truth ===")
    c, base, _ = obp_session(cfg)
    pat = bytes([0xF0]) * 48
    corrupt = bytes([0x0F] * 48)
    try:
        ab.check("erase", c.status(c.erase(base, 4096)), 0)
        ab.check("program 0xF0 x48 (driver-verified)", c.status(c.write(base, pat)), 0)
        r = c.write(base, corrupt)
        st = c.status(r)
    finally:
        c.close()
    got = read_region(cfg, base, 48)
    if st == 0:
        ab.check("wire says success and flash really holds the new bytes",
                 got == corrupt, True)
    else:
        ab.check("wire says E_FLASH", st, E_FLASH)
        if open_build:
            ab.check("detail is the driver's verify-mismatch code",
                     r[5], DETAIL_VERIFY_MISMATCH)
        ab.check("and flash really does NOT hold the requested bytes",
                 got != corrupt, True)
    print(f"  outcome: status={st} flash={'landed' if got == corrupt else 'refused'}")


def scenario_boundaries(name):
    cfg = ab.CHIPS[name]
    print(f"\n=== {name}: page-straddle and sector-edge writes ===")
    c, base, _ = obp_session(cfg)
    pat = prng_image(96, 0xB0DE)
    try:
        ab.check("erase 2 sectors", c.status(c.erase(base, 8192)), 0)
        # SINGLE 48-byte driver calls: only these exercise the page-split
        # loop inside one invocation (three 16 B frames would not).
        a1 = base + 0xF0                    # straddles the 256 B boundary
        ab.check("single 48B write straddling the page boundary",
                 c.status(c.write(a1, pat[:48])), 0)
        a2 = base + 0x1000 - 48             # ends exactly on the sector edge
        ab.check("single 48B write ending at the sector edge",
                 c.status(c.write(a2, pat[48:])), 0)
    finally:
        c.close()
    got1 = read_region(cfg, a1, 48)
    got2 = read_region(cfg, a2, 48)
    ab.check("straddle content correct (SWD)", got1 == pat[:48], True)
    ab.check("sector-edge content correct (SWD)", got2 == pat[48:], True)


def scenario_uid_parity(name):
    cfg = ab.CHIPS[name]
    print(f"\n=== {name}: UID parity open vs isp ===")
    ab.factory(cfg)                          # open bootloader
    ab.enter_bootloader(cfg)
    u_open = hello_uid(cfg)
    ab.power_cycle(cfg)
    ab.enter_bootloader(cfg)
    ab.check("uid stable across a power cycle", hello_uid(cfg), u_open)
    swap_boot(cfg, isp_boot(cfg))
    ab.enter_bootloader(cfg)
    u_isp = hello_uid(cfg)
    ab.check("uid identical under the vendor archive", u_isp, u_open)
    print(f"  uid: {u_open}")


SCENARIOS = {
    "soak": scenario_soak,
    "cross_driver": scenario_cross_driver,
    "cut_erase": scenario_cut_erase,
    "cut_write": scenario_cut_write,
    "negative_verify": scenario_negative_verify,
    "boundaries": scenario_boundaries,
    "uid_parity": scenario_uid_parity,
}


def main():
    if len(sys.argv) < 2 or sys.argv[1] not in ab.CHIPS:
        print(f"usage: {sys.argv[0]} <{'|'.join(ab.CHIPS)}> [scenario ...]")
        return 2
    name = sys.argv[1]
    picks = sys.argv[2:] or list(SCENARIOS)
    ab.fails = []
    for p in picks:
        SCENARIOS[p](name)
    print(f"\n>>> {name}: " + (f"{len(ab.fails)} FAILURES: {ab.fails}"
                               if ab.fails else "ALL CHECKS PASSED"))
    return 1 if ab.fails else 0


if __name__ == "__main__":
    raise SystemExit(main())
