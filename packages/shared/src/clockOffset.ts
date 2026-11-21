// Client clock-offset estimation.
//
// Every timestamp the credit system reads comes from the client's own clock,
// measured against a ±30s streak window inside a trust envelope (5 minutes
// behind server time, 5 seconds ahead). A system clock that is merely a few
// minutes off — common, invisible to the user, and none of their doing —
// therefore used to break recording outright: the server refused every
// upload-url request before issuing a presigned URL.
//
// The server now adopts its own time for any capture it can't trust, so the
// recording is never lost. This closes the other half: clients learn how far
// off they are from the server's own timestamps and correct their stamps, so
// a skewed clock costs nothing at all rather than costing precision.

/** Smallest offset worth correcting for. Below this, the "correction" would
 *  be indistinguishable from network jitter and would only add noise. */
export const CLOCK_OFFSET_DEADBAND_MS = 2_000;

/** A sample this far from the running estimate is a clock step (the OS
 *  syncing its time, a timezone fix, sleep/wake), not jitter: no round trip
 *  is that asymmetric. Smoothing it in would leave the stamps wrong for the
 *  better part of ten minutes — every one of them outside the credit window
 *  — so it is adopted outright, the same as a first sample. */
export const CLOCK_OFFSET_STEP_MS = 10_000;

/**
 * A running estimate of `serverNow - clientNow`.
 *
 * Deliberately tiny and dependency-free so both the web SDK and any other
 * client can share the arithmetic. Not a time sync protocol: we only need to
 * be well inside a 30-second window, and one sample per minute arrives for
 * free on every upload.
 */
export class ClockOffset {
  private offsetMs = 0;
  private samples = 0;

  /**
   * Fold in one observation.
   *
   * `serverTime` is the server's clock when it handled the request, and
   * `requestSentAtMs`/`responseReceivedAtMs` bracket it on the local clock.
   * The server's timestamp was taken somewhere inside that window, so the
   * local instant that best corresponds to it is the midpoint — which removes
   * most of the round trip from the estimate rather than charging all of it to
   * the offset. This is the same reasoning NTP uses, minus the rigour we
   * don't need.
   */
  observe(
    serverTime: string,
    requestSentAtMs: number,
    responseReceivedAtMs: number,
  ): void {
    const serverMs = Date.parse(serverTime);
    if (!Number.isFinite(serverMs)) return;
    // A negative or absurd interval means the local clock moved under us
    // mid-request (NTP correction, sleep/wake). Treat the sample as
    // untrustworthy rather than folding a jump into the estimate.
    if (responseReceivedAtMs < requestSentAtMs) return;

    const localMidpoint =
      requestSentAtMs + (responseReceivedAtMs - requestSentAtMs) / 2;
    const sample = serverMs - localMidpoint;

    // First sample is adopted outright — a badly wrong clock should be
    // corrected on the very next capture, not eased into over many minutes.
    // So is a clock step, for the same reason. Everything else is smoothed,
    // so ordinary jitter doesn't wobble the stamps we send.
    const isStep = Math.abs(sample - this.offsetMs) >= CLOCK_OFFSET_STEP_MS;
    this.offsetMs =
      this.samples === 0 || isStep
        ? sample
        : this.offsetMs * 0.75 + sample * 0.25;
    this.samples++;
  }

  /** Current estimate of how far the local clock is behind the server's. */
  get offset(): number {
    return this.samples === 0 ? 0 : this.offsetMs;
  }

  /** True once an offset large enough to matter has been observed. */
  get isSignificant(): boolean {
    return Math.abs(this.offset) >= CLOCK_OFFSET_DEADBAND_MS;
  }

  /**
   * Correct a local timestamp into server time.
   *
   * A no-op inside the deadband, so a healthy client's timestamps are passed
   * through byte-identical and nothing about its behaviour changes.
   */
  correct(localMs: number): number {
    return this.isSignificant ? Math.round(localMs + this.offset) : localMs;
  }
}
