// Captures the main screens with a headless Chrome. Needs `npm run build` first.
// Uses the locally installed Google Chrome through playwright-core (no browser download).
import { chromium } from "playwright-core";
import { preview } from "vite";
import { mkdirSync } from "node:fs";

const SHOTS = [
  ["01-status-jobs", "notes-assistant/status"],
  ["02-daemon-switcher", "notes-assistant/status?switcher=1"],
  ["03a-tray-menu", "notes-assistant/status?tray=1"],
  ["03b-os-notifications", "notes-assistant/status?notify=update,refused"],
  ["04-job-editor-form", "notes-assistant/job/weekly-review"],
  ["05-job-editor-raw", "notes-assistant/job/inbox-triage", async (p) => p.getByRole("button", { name: "Edit raw markdown" }).click()],
  ["06-chat-scope-picker", "notes-assistant/chat", async (p) => p.getByRole("button", { name: /Global session/ }).first().click()],
  ["07-chat-refused-launcher", "notes-assistant/chat/ch-research", async (p) => {
    await p.getByPlaceholder(/Message/).fill("Summarise the last discussion");
    await p.keyboard.press("Enter");
    await p.waitForTimeout(700);
  }],
  ["08-sessions", "notes-assistant/sessions"],
  ["09-launchers-test", "notes-assistant/launchers", async (p) => {
    await p.getByRole("button", { name: "Test launcher" }).nth(1).click();
    await p.waitForTimeout(1100);
  }],
  ["10-launchers-refused", "notes-assistant/launchers", async (p) => {
    await p.getByRole("button", { name: "Test launcher" }).nth(2).click();
    await p.waitForTimeout(1100);
  }],
  ["11-settings-remote", "notes-assistant/settings/remote"],
  ["12-logs", "notes-assistant/logs"],
  ["13-usage", "notes-assistant/usage"],
  ["14-discord", "notes-assistant/discord"],
  ["15-agents", "notes-assistant/agents"],
  ["16-error-daemon-down", "home-lab/status"],
  ["17-error-crash-loop", "studio-mini/status?notify=crash"],
  ["18-error-unreachable", "pi-garden/status"],
  ["19-offline-screen", "home-lab/chat"],
];

const server = await preview({ preview: { port: 4179, strictPort: true } });
const browser = await chromium.launch({ channel: "chrome", headless: true });
const page = await browser.newPage({ viewport: { width: 1440, height: 900 }, deviceScaleFactor: 1 });
mkdirSync("screenshots", { recursive: true });
for (const [name, hash, act] of SHOTS) {
  await page.goto(`http://localhost:4179/#/${hash}`);
  await page.reload();
  await page.waitForTimeout(300);
  if (act) await act(page);
  await page.waitForTimeout(150);
  await page.screenshot({ path: `screenshots/${name}.png` });
  console.log("saved", name);
}
await browser.close();
server.httpServer.close();
