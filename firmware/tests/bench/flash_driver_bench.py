#!/usr/bin/env python3
"""Hardware acceptance bench for the open flash driver (ports/flash_ch5xx.c).

Companion to ab_bench.py (whose helpers this reuses), covering what the A/B
state-machine scenarios do not: the driver's own erase/write/verify on real
silicon, interruption INSIDE a flash operation, and verification that does
not trust the driver with its own homework.

Scenarios (per chip):
  soak            PRNG image through the driver's raw erase/write; readback
                  over SWD (driver-independent); live CRC; cold power-cycle
                  re-check.
  cut_erase       power cut aimed inside the whole-window erase sequence
                  (aim self-calibrated from the setup erase); every sector
                  classified post-mortem from a full SWD dump; requires
                  >= MIN_MIDOP mid-sequence landings and full recovery.
  cut_write       power cut inside a paced page-program run; the frontier
                  shape is enforced (classifier soundness) and at least one
                  TORN program op must be observed across the runs.
  negative_verify PRNG-over-PRNG without an erase; the wire report must
                  match SWD ground truth (E_FLASH + verify-mismatch detail
                  when the die refuses, exact content when it lands).
  boundaries      a 48 B write straddling a 256 B page boundary and one
                  ending exactly at a sector boundary, SWD-compared.
  uid_stability   HELLO uid nonzero and stable across a power cycle.

History: until the libISP cutover this file also carried cross_driver and
uid_parity, which compared the open driver against the vendor-archive build
per-die; that side-by-side evidence (CRC parity in both directions and
byte-identical uids on CH592 and CH570, both clock regimes) is archived in
the cutover PR.

Requires: a WCH-LinkE powering the target (ab_bench.py CHIPS table) and the
bootloader image built: gmake CHIP=<c> TRANSPORT=uart [BOARD=...]
Override the minichlink path with MINICHLINK=... if ab_bench.py's default
does not exist on this machine.

Usage: flash_driver_bench.py <ch570|ch572|ch592> [scenario ...]
"""
import os
import random
import struct
import sys
import threading
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
    img = prng_image(cap, 0x5EED)            # cap already excludes the record
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


def _recover_full_sector(cfg, victim, seed):
    """Recovery after a cut is only proven by reprogramming the WHOLE
    interrupted sector (`victim`, absolute address), SWD-comparing it,
    power-cycling, and comparing again — recovering some other healthy
    sector proves nothing, and a 16-byte token would miss weak bits
    anywhere else in the sector."""
    act, wr, base, _ = ab.enter_bootloader(cfg)
    pat = prng_image(4096, seed)
    c = ab.Obp(cfg["port"])
    try:
        c.hello()
        if c.status(c.erase(victim, 4096)) != 0:
            return False
        for off in range(0, 4096, 48):
            if c.status(c.write(victim + off, pat[off:off + 48])) != 0:
                return False
    finally:
        c.close()
    if read_region(cfg, victim, 4096) != pat:
        return False
    ab.power_cycle(cfg)
    return read_region(cfg, victim, 4096) == pat


# minichlink -t spawn-to-exit, measured on this bench: the fixed part of the
# cut path that a scenario must subtract when it aims inside a short window.
MC_CUT_LATENCY = 0.028


def _cut_power(cfg, c, delay, close=True):
    """Cut 3V3 with the serial port still OPEN: closing the CDC port resets
    the target (bench README), which would abort the op before the cut ever
    lands. TX is held in break through the off window — this fixture's
    idle-high TX back-powers the chip through the RX pin's clamp diode
    otherwise, and a 'cut' that leaves the die powered is not a cut."""
    time.sleep(delay)
    c.s.break_condition = True
    ab.mc(cfg, "-t", t=30)
    time.sleep(0.6)
    ab.mc(cfg, "-3", t=30)
    c.s.break_condition = False
    if close:
        c.close()
    time.sleep(2.0)


def scenario_cut_erase(name):
    """Power cut inside a WHOLE-WINDOW erase sequence. A single 4 KiB
    sector erase completes in ~5 ms — far inside the cut path's latency —
    so the aimable window is the multi-sector SEQUENCE (measured 951 ms
    for the 53-sector window on this fixture). Setup leaves a 48 B PRNG
    stamp on every erased sector; the post-mortem classifies each sector
    from a full SWD dump and the trial from the mix: all-erased = cut
    after completion, all-stamped = before it started, a mix (or any torn
    sector) = the cut landed inside the erase sequence."""
    cfg = ab.CHIPS[name]
    print(f"\n=== {name}: power cuts inside a multi-sector erase sequence ===")
    classes = {"pre": 0, "mid": 0, "post": 0}
    er4 = ERASED_WORD.to_bytes(4, "little")
    for t in range(TRIALS):
        c, base, cap = obp_session(cfg)
        span = cap                           # cap already excludes the record
        nsec = span // 4096
        stamps = [prng_image(48, (0xA000 + t) * 64 + s) for s in range(nsec)]
        try:
            # The setup erase doubles as the aim calibration: the sequence
            # duration is family- and die-dependent (bench: ~950 ms for 53
            # sectors on a CH592 at 6.4 MHz, ~74 ms for 27 on a CH570 —
            # 7x faster per sector), so a hardcoded window overshoots one
            # chip or the other.
            t0 = time.time()
            if c.status(c.xfer(0x02, struct.pack("<II", base, span),
                               t=30.0)) != 0:
                raise RuntimeError("setup erase failed")
            t_seq = time.time() - t0
            for s in range(nsec):
                if c.status(c.write(base + s * 4096, stamps[s])) != 0:
                    raise RuntimeError("setup stamp failed")
            # Fire the whole-window ERASE without waiting, then cut inside
            # the just-measured sequence window. mutation_begin may touch
            # the record block first, so where the cut lands is CLASSIFIED,
            # never assumed.
            c.s.write(ab.frame(0x02, 1, struct.pack("<II", base, span)))
            c.s.flush()
        except Exception:
            c.close()
            raise
        _cut_power(cfg, c, max(0.0, random.uniform(0.0, 0.95 * t_seq)
                               - MC_CUT_LATENCY))
        got = read_region(cfg, base, span)
        n_er = n_st = n_torn = 0
        first_torn = last_erased = None
        for s in range(nsec):
            sec = got[s * 4096:(s + 1) * 4096]
            if all(sec[i:i + 4] == er4 for i in range(0, 4096, 4)):
                n_er += 1
                last_erased = s
            elif sec[:48] == stamps[s] and all(
                    sec[i:i + 4] == er4 for i in range(48, 4096, 4)):
                n_st += 1
            else:
                n_torn += 1
                if first_torn is None:
                    first_torn = s
        cls = "post" if n_st == 0 and n_torn == 0 else \
              "pre" if n_er == 0 and n_torn == 0 else "mid"
        classes[cls] += 1
        # Recovery must target the INTERRUPTED sector: the first torn one,
        # else the newest sector the sequence touched — recovering some
        # other healthy sector would prove nothing.
        v = first_torn if first_torn is not None else \
            (last_erased if last_erased is not None else 0)
        print(f"  trial {t}: {cls} (erased={n_er} stamped={n_st} "
              f"torn={n_torn}, recovering sector {v})")
        ab.check(f"trial {t} ({cls}): full-sector recovery",
                 _recover_full_sector(cfg, base + v * 4096, 0xAA00 + t), True)
    print(f"  cut classes over {TRIALS} trials: {classes}")
    ab.check("enough cuts landed mid-erase-sequence",
             classes["mid"] >= MIN_MIDOP, True)


def scenario_cut_write(name):
    """Power cut inside a paced page-program run. One 48 B program op is
    ~14 ms frame-to-response — not aimable through the ~30 ms cut path on
    its own — so a worker thread fills the sector with NORMAL paced writes
    (the device spends ~60% of that ~1.2 s inside the driver) and the cut
    fires at a random instant of the run. Fire-and-forget streaming is
    deliberately NOT used: the transport is fully polled and the 8-byte
    UART FIFO overflows while the driver runs from RAM, so a full-rate
    stream just sheds frames. Post-mortem walks the spans in write order:
    a complete prefix, at most one torn span at the frontier, then erased
    tail — anything after the frontier is a classifier-soundness failure.
    Torn landings are die/aim luck (prior bench: 1 in 52), so extra runs
    are allowed until one is observed."""
    cfg = ab.CHIPS[name]
    print(f"\n=== {name}: power cut inside a paced page-program run ===")
    er4 = ERASED_WORD.to_bytes(4, "little")
    spans = list(range(0, 4096, 48))
    mid = torn_total = bad_total = trials = 0
    while trials < TRIALS or (torn_total == 0 and trials < TRIALS * 4):
        t = trials
        c, base, cap = obp_session(cfg)
        pat = prng_image(4096, 0xB000 + t)
        try:
            if c.status(c.erase(base, 4096)) != 0:
                raise RuntimeError("setup erase failed")

            def fill():
                for off in spans:
                    if c.status(c.write(base + off, pat[off:off + 48])) != 0:
                        return               # the cut landed: stop quietly

            w = threading.Thread(target=fill)
            w.start()
            _cut_power(cfg, c, random.uniform(0.02, 1.1), close=False)
            w.join(10.0)
        finally:
            c.close()
        got = read_region(cfg, base, 4096)
        n_complete = torn = bad = 0
        state = "run"
        for off in spans:
            n = min(48, 4096 - off)
            spanb = got[off:off + n]
            want = pat[off:off + n]
            erased = all(spanb[i:i + 4] == er4 for i in range(0, n, 4))
            if spanb == want:
                n_complete += 1
                if state != "run":
                    bad += 1
            elif erased:
                state = "tail"
            else:
                torn += 1
                if state != "run":
                    bad += 1
                state = "tail"
        incomplete = n_complete < len(spans)
        mid += incomplete
        torn_total += torn
        bad_total += bad
        tag = "TORN" if torn else ("mid-run" if incomplete else "complete")
        print(f"  trial {t}: {tag} (complete={n_complete}/{len(spans)} "
              f"torn={torn} bad={bad})")
        ab.check(f"trial {t} ({tag}): full-sector recovery",
                 _recover_full_sector(cfg, base, 0xBB00 + t), True)
        trials += 1
    print(f"  mid-run landings {mid}/{trials}, torn program ops {torn_total}")
    ab.check("classifier soundness (nothing written past the frontier)",
             bad_total, 0)
    ab.check("enough cuts landed inside the program run",
             mid >= MIN_MIDOP, True)
    ab.check("at least one torn program op observed", torn_total >= 1, True)


def scenario_negative_verify(name, open_build=True):
    """Verify-honesty check: program PRNG pattern A into erased cells
    (landed, driver-verified), then attempt PRNG pattern B over it without
    an erase. Programming over programmed cells cannot reproduce an
    arbitrary new pattern, so the driver's post-program read must see a
    mismatch and report E_FLASH with the verify-mismatch detail, with SWD
    confirming B is not what flash holds. The obvious 0xF0->0x0F nibble
    swap is deliberately NOT used: CH5xx cells are scrambled and
    overprogram results are flash-timing- and read-path-dependent — bench
    evidence has that transition genuinely landing at UART reset timing,
    and landing 383/384 bits at 60 MHz with the last cell reading 0x0F
    through the controller but 0x07 over SWD (a marginal cell, not a
    driver bug). Random-over-random leaves no such escape. Should the die
    ever genuinely land B (st == 0), the wire must still match SWD."""
    cfg = ab.CHIPS[name]
    print(f"\n=== {name}: verify report must match SWD ground truth ===")
    c, base, _ = obp_session(cfg)
    pat = prng_image(48, 0x0E01)
    corrupt = prng_image(48, 0x0E02)
    try:
        ab.check("erase", c.status(c.erase(base, 4096)), 0)
        ab.check("program PRNG A x48 (driver-verified)",
                 c.status(c.write(base, pat)), 0)
        r = c.write(base, corrupt)
        st = c.status(r)
    finally:
        c.close()
    got = read_region(cfg, base, 48)
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


def scenario_uid_stability(name):
    """The uid must be a real device identity: nonzero (the CH570 MAC
    fallback included) and identical across a power cycle. Byte-parity
    against the vendor archive was proven before the libISP cutover and is
    archived in that PR."""
    cfg = ab.CHIPS[name]
    print(f"\n=== {name}: UID nonzero and stable ===")
    ab.factory(cfg)
    ab.enter_bootloader(cfg)
    u1 = hello_uid(cfg)
    ab.check("uid is nonzero", u1 != "0" * 16, True)
    ab.power_cycle(cfg)
    ab.enter_bootloader(cfg)
    ab.check("uid stable across a power cycle", hello_uid(cfg), u1)
    print(f"  uid: {u1}")


SCENARIOS = {
    "soak": scenario_soak,
    "cut_erase": scenario_cut_erase,
    "cut_write": scenario_cut_write,
    "negative_verify": scenario_negative_verify,
    "boundaries": scenario_boundaries,
    "uid_stability": scenario_uid_stability,
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
