import { shiftDateToOffset } from "./timezone";

function matchCronField(field: string, value: number): boolean {
  for (const part of field.split(",")) {
    const [range, stepStr] = part.split("/");
    const step = stepStr ? parseInt(stepStr) : 1;

    if (range === "*") {
      if (value % step === 0) return true;
      continue;
    }

    if (range.includes("-")) {
      const [lo, hi] = range.split("-").map(Number);
      if (value >= lo && value <= hi && (value - lo) % step === 0) return true;
      continue;
    }

    if (parseInt(range) === value) return true;
  }
  return false;
}

/** A schedule must have exactly five fields: minute hour day-of-month month day-of-week. */
export function isValidCronExpr(expr: string): boolean {
  return expr.trim().split(/\s+/).length === 5;
}

export function cronMatches(expr: string, date: Date, timezoneOffsetMinutes = 0): boolean {
  // A malformed expression used to throw here, inside the scheduler tick, which skipped
  // every remaining job for that tick. Job loading rejects these; never match defensively.
  if (!isValidCronExpr(expr)) return false;
  const [minute, hour, dayOfMonth, month, dayOfWeek] = expr.trim().split(/\s+/);
  const shifted = shiftDateToOffset(date, timezoneOffsetMinutes);
  const d = {
    minute: shifted.getUTCMinutes(),
    hour: shifted.getUTCHours(),
    dayOfMonth: shifted.getUTCDate(),
    month: shifted.getUTCMonth() + 1,
    dayOfWeek: shifted.getUTCDay(),
  };

  return (
    matchCronField(minute, d.minute) &&
    matchCronField(hour, d.hour) &&
    matchCronField(dayOfMonth, d.dayOfMonth) &&
    matchCronField(month, d.month) &&
    matchCronField(dayOfWeek, d.dayOfWeek)
  );
}

export function nextCronMatch(expr: string, after: Date, timezoneOffsetMinutes = 0): Date {
  const d = new Date(after);
  d.setSeconds(0, 0);
  d.setMinutes(d.getMinutes() + 1);
  for (let i = 0; i < 2880; i++) {
    if (cronMatches(expr, d, timezoneOffsetMinutes)) return d;
    d.setMinutes(d.getMinutes() + 1);
  }
  return d;
}

/**
 * Most recent time at or before `before` that matches `expr`, or null if there is
 * none within `withinMinutes`.
 *
 * Used to detect a missed run: the scheduler only polls once a minute and asks
 * "does this minute match", so a run whose minute passed while the daemon was down
 * -- or while the event loop was blocked long enough to skip a tick -- is otherwise
 * lost silently. The bound keeps a long outage from resurrecting days of backlog.
 */
export function prevCronMatch(
  expr: string,
  before: Date,
  timezoneOffsetMinutes = 0,
  withinMinutes = 360,
): Date | null {
  const d = new Date(before);
  d.setSeconds(0, 0);
  for (let i = 0; i <= withinMinutes; i++) {
    if (cronMatches(expr, d, timezoneOffsetMinutes)) return new Date(d);
    d.setMinutes(d.getMinutes() - 1);
  }
  return null;
}
