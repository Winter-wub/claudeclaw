// Tiny cron matcher for the next-run preview. Follows ClaudeClaw's semantics:
// all five fields are ANDed (day-of-month AND day-of-week), `*/n` means value % n == 0,
// names and `7` are not supported. Times are UTC, which stands in for the daemon's timezone.

const RANGES: [number, number][] = [
  [0, 59],
  [0, 23],
  [1, 31],
  [1, 12],
  [0, 6],
];
const FIELD_NAMES = ["minute", "hour", "day of month", "month", "day of week"];

export function validateCron(expr: string): string | null {
  const parts = expr.trim().split(/\s+/);
  if (parts.length !== 5) return `Needs 5 fields (minute hour day month weekday), got ${parts.length}.`;
  for (let i = 0; i < 5; i++) {
    for (const p of parts[i].split(",")) {
      if (!/^(\*|\d+(-\d+)?)(\/\d+)?$/.test(p)) {
        if (/[a-z]/i.test(p)) return `${FIELD_NAMES[i]}: names like "${p}" never match. Use numbers (0 = Sunday).`;
        return `${FIELD_NAMES[i]}: "${p}" is not valid.`;
      }
      const nums = p.match(/\d+/g)?.map(Number) ?? [];
      const [lo, hi] = RANGES[i];
      const vals = p.includes("/") ? nums.slice(0, -1) : nums;
      if (vals.some((n) => n < lo || n > hi)) return `${FIELD_NAMES[i]}: values must be ${lo}–${hi}.`;
      if (/^\d+\/\d+$/.test(p)) return `${FIELD_NAMES[i]}: "${p}" ignores the step and matches only ${nums[0]}.`;
    }
  }
  return null;
}

function fieldMatches(field: string, value: number): boolean {
  return field.split(",").some((part) => {
    const [range, stepStr] = part.split("/");
    const step = stepStr ? Number(stepStr) : 1;
    if (range === "*") return value % step === 0;
    const [a, b] = range.split("-").map(Number);
    if (b === undefined) return value === a;
    return value >= a && value <= b && (value - a) % step === 0;
  });
}

export function nextRuns(expr: string, count = 5, from = new Date()): Date[] {
  if (validateCron(expr)) return [];
  const f = expr.trim().split(/\s+/);
  const out: Date[] = [];
  const t = new Date(from);
  t.setUTCSeconds(0, 0);
  t.setUTCMinutes(t.getUTCMinutes() + 1);
  const limit = from.getTime() + 400 * 24 * 3600 * 1000;
  while (out.length < count && t.getTime() < limit) {
    const dayOk =
      fieldMatches(f[2], t.getUTCDate()) && fieldMatches(f[3], t.getUTCMonth() + 1) && fieldMatches(f[4], t.getUTCDay());
    if (!dayOk) {
      t.setUTCDate(t.getUTCDate() + 1);
      t.setUTCHours(0, 0, 0, 0);
      continue;
    }
    if (!fieldMatches(f[1], t.getUTCHours())) {
      t.setUTCHours(t.getUTCHours() + 1, 0, 0, 0);
      continue;
    }
    if (fieldMatches(f[0], t.getUTCMinutes())) out.push(new Date(t));
    t.setUTCMinutes(t.getUTCMinutes() + 1);
  }
  return out;
}

const DAYS = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];

/** Best-effort plain-English description of common shapes. */
export function describeCron(expr: string): string {
  const f = expr.trim().split(/\s+/);
  if (f.length !== 5 || validateCron(expr)) return "Custom schedule";
  const [m, h, dom, mon, dow] = f;
  const pad = (n: string) => n.padStart(2, "0");
  if (m.startsWith("*/") && h === "*" && dom === "*" && mon === "*" && dow === "*") return `Every ${m.slice(2)} minutes`;
  if (m.startsWith("*/") && /^\d+-\d+$/.test(h) && dom === "*" && mon === "*" && dow === "*")
    return `Every ${m.slice(2)} min, ${h.replace("-", ":00–")}:59`;
  if (/^\d+$/.test(m) && h === "*" && dom === "*" && mon === "*" && dow === "*") return `Hourly at :${pad(m)}`;
  if (/^\d+$/.test(m) && /^\d+$/.test(h)) {
    const time = `${pad(h)}:${pad(m)}`;
    if (dom === "*" && mon === "*" && dow === "*") return `Daily at ${time}`;
    if (dom === "*" && mon === "*" && dow === "1-5") return `Weekdays at ${time}`;
    if (dom === "*" && mon === "*" && /^[\d,]+$/.test(dow)) return `${dow.split(",").map((d) => DAYS[+d]).join(", ")} at ${time}`;
    if (/^\d+$/.test(dom) && mon === "*" && dow === "*") return `Monthly on day ${dom} at ${time}`;
    if (/^\d+$/.test(dom) && /^\d+$/.test(mon) && dow === "*")
      return `${new Date(Date.UTC(2026, +mon - 1, +dom)).toLocaleDateString("en-US", { month: "short", day: "numeric", timeZone: "UTC" })} at ${time}`;
  }
  return "Custom schedule";
}

export function bothDayFieldsRestricted(expr: string) {
  const f = expr.trim().split(/\s+/);
  return f.length === 5 && f[2] !== "*" && f[4] !== "*";
}

export function fmtRun(d: Date) {
  return d.toLocaleString("en-GB", {
    weekday: "short",
    day: "numeric",
    month: "short",
    hour: "2-digit",
    minute: "2-digit",
    timeZone: "UTC",
  });
}
