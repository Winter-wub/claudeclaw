import { describe, it, expect } from "bun:test";
import { prevCronMatch, cronMatches, nextCronMatch, isValidCronExpr } from "../cron";

// +07 (Asia/Bangkok) is the daemon's configured offset.
const TZ = 420;

describe("prevCronMatch", () => {
  it("finds the occurrence that just passed", () => {
    // 13:00 +07 == 06:00Z. Ask at 06:12Z, the real 2026-09-15 miss.
    const p = prevCronMatch("0 13 * * *", new Date("2026-09-15T06:12:40Z"), TZ);
    expect(p?.toISOString()).toBe("2026-09-15T06:00:00.000Z");
  });

  it("returns the current minute when it matches", () => {
    const p = prevCronMatch("0 13 * * *", new Date("2026-09-15T06:00:30Z"), TZ);
    expect(p?.toISOString()).toBe("2026-09-15T06:00:00.000Z");
  });

  it("returns null past the window, so an outage cannot resurrect a backlog", () => {
    // 7h later, window is 360 minutes
    expect(prevCronMatch("0 13 * * *", new Date("2026-09-15T13:00:00Z"), TZ)).toBeNull();
  });

  it("honours a custom window", () => {
    const at = new Date("2026-09-15T06:30:00Z");
    expect(prevCronMatch("0 13 * * *", at, TZ, 60)).not.toBeNull();
    expect(prevCronMatch("0 13 * * *", at, TZ, 10)).toBeNull();
  });

  it("crosses midnight correctly", () => {
    // 03:00 +07 == 20:00Z previous day (the dreaming job's shape)
    const p = prevCronMatch("0 3 * * *", new Date("2026-09-15T20:40:00Z"), TZ);
    expect(p?.toISOString()).toBe("2026-09-15T20:00:00.000Z");
  });

  it("agrees with cronMatches at the instant it returns", () => {
    const p = prevCronMatch("*/15 * * * *", new Date("2026-09-15T06:07:00Z"), TZ);
    expect(p).not.toBeNull();
    expect(cronMatches("*/15 * * * *", p!, TZ)).toBe(true);
  });
});

describe("catch-up decision", () => {
  // Mirrors the scheduler's rule: fire when the occurrence that just passed is
  // newer than the job's recorded last run.
  const shouldCatchUp = (schedule: string, now: Date, lastRanAt: number) => {
    const due = prevCronMatch(schedule, now, TZ, 360);
    if (!due) return false;
    return lastRanAt < due.getTime();
  };

  const DUE = new Date("2026-09-15T06:00:00Z").getTime();

  it("fires when the daemon was down across the scheduled minute", () => {
    const bootedAt = new Date("2026-09-15T06:12:40Z");
    const ranYesterday = new Date("2026-09-14T06:00:00Z").getTime();
    expect(shouldCatchUp("0 13 * * *", bootedAt, ranYesterday)).toBe(true);
  });

  it("does NOT re-fire a job that already ran this occurrence", () => {
    // the restart-duplication case: seeded lastRanAt must suppress it
    expect(shouldCatchUp("0 13 * * *", new Date("2026-09-15T06:12:40Z"), DUE)).toBe(false);
    expect(shouldCatchUp("0 13 * * *", new Date("2026-09-15T06:12:40Z"), DUE + 5_000)).toBe(false);
  });

  it("does not fire before the first occurrence of the day", () => {
    expect(shouldCatchUp("0 13 * * *", new Date("2026-09-15T05:30:00Z"), DUE - 86_400_000)).toBe(false);
  });

  it("gives up rather than firing a very stale run", () => {
    const ranYesterday = new Date("2026-09-14T06:00:00Z").getTime();
    expect(shouldCatchUp("0 13 * * *", new Date("2026-09-15T13:00:00Z"), ranYesterday)).toBe(false);
  });
});

describe("in-flight guard", () => {
  // Mirrors the scheduler's combined decision: fire on an exact cron match OR a
  // still-due catch-up occurrence, but skip either while the job is already running.
  // This is what stops a job whose run outlasts one 60s tick from being re-fired every
  // tick until it finishes -- dreaming fired 3x on 2026-09-17, fs-sr62454-approval-check
  // 29 of 41 total runs, before this guard existed.
  const shouldFire = (schedule: string, now: Date, lastRanAt: number, inFlight: boolean) => {
    if (inFlight) return false;
    if (cronMatches(schedule, now, TZ)) return true;
    const due = prevCronMatch(schedule, now, TZ, 360);
    return !!due && lastRanAt < due.getTime();
  };

  it("suppresses catch-up while a long-running job from the same occurrence is still in flight", () => {
    // 03:00 +07 == 20:00Z -- the dreaming job's schedule. Two ticks later it hasn't
    // finished (lastRanAt is still 0), and without the guard this re-fires every tick.
    const stillRunning = new Date("2026-09-17T20:02:00Z");
    expect(shouldFire("0 3 * * *", stillRunning, 0, true)).toBe(false);
  });

  it("fires the catch-up normally once the job is no longer in flight", () => {
    const stillDue = new Date("2026-09-17T20:02:00Z");
    expect(shouldFire("0 3 * * *", stillDue, 0, false)).toBe(true);
  });
});

describe("malformed schedules", () => {
  // A schedule with fewer than five fields used to throw a TypeError inside the 60s
  // tick, skipping every other job for that tick (and throwing again at boot).
  it("is rejected by isValidCronExpr", () => {
    expect(isValidCronExpr("0 13 * * *")).toBe(true);
    expect(isValidCronExpr("  */5  *  * * *  ")).toBe(true);
    expect(isValidCronExpr("0 13 * *")).toBe(false);
    expect(isValidCronExpr("")).toBe(false);
    expect(isValidCronExpr("0 0 13 * * *")).toBe(false);
  });

  it("never matches and never throws", () => {
    const now = new Date("2026-09-15T06:00:00Z");
    expect(() => cronMatches("0 13 * *", now, TZ)).not.toThrow();
    expect(cronMatches("0 13 * *", now, TZ)).toBe(false);
    expect(prevCronMatch("0 13", now, TZ, 60)).toBeNull();
  });
});
