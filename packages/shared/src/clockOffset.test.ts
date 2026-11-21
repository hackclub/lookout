import { describe, expect, it } from "vitest";
import { ClockOffset, CLOCK_OFFSET_DEADBAND_MS } from "./clockOffset.js";

/**
 * The client half of clock-skew tolerance. The server adopts its own time for
 * a capture it can't trust, which stops a wrong clock costing the recording;
 * this is what stops it costing precision too.
 */
describe("ClockOffset", () => {
  const iso = (ms: number) => new Date(ms).toISOString();

  it("is a no-op before it has seen anything", () => {
    const c = new ClockOffset();
    expect(c.offset).toBe(0);
    expect(c.isSignificant).toBe(false);
    expect(c.correct(1_000)).toBe(1_000);
  });

  it("leaves a healthy clock's timestamps byte-identical", () => {
    // Well inside the deadband: correcting here would add noise, not accuracy,
    // and would make every healthy client's behaviour depend on jitter.
    const c = new ClockOffset();
    const local = 1_700_000_000_000;
    c.observe(iso(local + 300), local, local + 100);
    expect(c.isSignificant).toBe(false);
    expect(c.correct(local)).toBe(local);
  });

  it("corrects a clock that is minutes SLOW on the first sample", () => {
    // First sample is adopted outright — a badly wrong clock must be fixed on
    // the very next capture, not eased into over ten minutes of lost credit.
    const c = new ClockOffset();
    const local = 1_700_000_000_000;
    const skew = 7 * 60_000;
    c.observe(iso(local + skew), local, local + 40);
    expect(c.isSignificant).toBe(true);
    expect(c.correct(local)).toBeCloseTo(local + skew, -2);
  });

  it("corrects a clock that is minutes FAST", () => {
    const c = new ClockOffset();
    const local = 1_700_000_000_000;
    const skew = -9 * 60_000;
    c.observe(iso(local + skew), local, local + 40);
    expect(c.correct(local)).toBeCloseTo(local + skew, -2);
  });

  it("charges the round trip to latency, not to the offset", () => {
    // The server's timestamp was taken somewhere inside the request window,
    // so the midpoint is the honest local counterpart. Attributing the whole
    // round trip to skew would bias every estimate by half the RTT — which on
    // a slow link is exactly the population we most need to be right about.
    const c = new ClockOffset();
    const local = 1_700_000_000_000;
    const rtt = 4_000;
    // A perfectly synced clock, observed over a slow request.
    c.observe(iso(local + rtt / 2), local, local + rtt);
    expect(Math.abs(c.offset)).toBeLessThan(CLOCK_OFFSET_DEADBAND_MS);
    expect(c.isSignificant).toBe(false);
  });

  it("smooths later samples so jitter doesn't wobble the stamps", () => {
    const c = new ClockOffset();
    const local = 1_700_000_000_000;
    const skew = 60_000;
    c.observe(iso(local + skew), local, local + 20);
    const afterFirst = c.offset;
    // One noisy sample must not move the estimate far.
    c.observe(iso(local + skew + 5_000), local, local + 20);
    expect(c.offset).toBeGreaterThan(afterFirst);
    expect(c.offset).toBeLessThan(afterFirst + 5_000);
  });

  it("adopts a clock step outright instead of easing into it", () => {
    // The OS correcting the clock by minutes mid-recording. Smoothed at 25%
    // a sample, the estimate would stay outside the ±30s credit window for
    // six more captures — six minutes of credit lost to a correction the
    // user never saw.
    const c = new ClockOffset();
    const local = 1_700_000_000_000;
    c.observe(iso(local), local, local + 20);
    const stepped = local + 3 * 60_000; // the clock jumped 3 minutes ahead
    c.observe(iso(local + 60_000), stepped + 60_000, stepped + 60_020);
    expect(c.correct(stepped + 60_000)).toBeCloseTo(local + 60_000, -2);
  });

  it("ignores a sample whose local window ran backwards", () => {
    // The local clock being corrected (NTP, sleep/wake) mid-request would
    // otherwise fold a jump straight into the estimate.
    const c = new ClockOffset();
    const local = 1_700_000_000_000;
    c.observe(iso(local + 60_000), local, local - 5_000);
    expect(c.offset).toBe(0);
  });

  it("ignores an unparseable server timestamp", () => {
    const c = new ClockOffset();
    c.observe("not a date", 1_000, 1_010);
    expect(c.offset).toBe(0);
  });

  it("brings a skewed capture inside the credit window", () => {
    // End to end: a device 6 minutes fast is outside the ±5min envelope, so
    // every capture would have been refused. After one observation its stamps
    // land within a second of server time — comfortably inside the ±30s
    // streak window.
    const c = new ClockOffset();
    const serverMs = 1_700_000_000_000;
    const deviceMs = serverMs + 6 * 60_000;
    c.observe(iso(serverMs), deviceMs, deviceMs + 50);
    const corrected = c.correct(deviceMs);
    expect(Math.abs(corrected - serverMs)).toBeLessThan(1_000);
  });
});
