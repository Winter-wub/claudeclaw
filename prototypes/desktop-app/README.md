# Desktop app prototype (throwaway)

A clickable mock of the ClaudeClaw **Desktop app**, built for [#16 Prototype the desktop app's screens and navigation](https://github.com/Winter-wub/claudeclaw/issues/16). It exists only so the maintainer can react to the layout and navigation before the real Tauri app is built. Everything is mock data, there is no backend and no Tauri, and the code will not be reused.

Stack, matching [#10](https://github.com/Winter-wub/claudeclaw/issues/10): Vite + React + TypeScript + Tailwind, with lucide icons. It uses shadcn-style hand-rolled components rather than shadcn/ui itself, to keep it small.

## Run it

```sh
cd prototypes/desktop-app
npm install
npm run dev        # http://localhost:5173
npm run build      # type-check + production build into dist/
```

## Screenshots

```sh
npm run build && npm run screenshots   # writes screenshots/*.png
```

The script uses `playwright-core` and the locally installed Google Chrome (`channel: "chrome"`), so it downloads no browser.

## What to click

- **Daemon switcher** (top of the sidebar): three local and two paired remote daemons, each in a different state. Local daemons carry a per-daemon "start at login" flag, which you can toggle in the switcher, on Status, or in Settings → Startup & window.
  - `notes-assistant`: running, with one job refused because of its launcher.
  - `scratch-bot`: running but started from the CLI, so the app is not watching it. Use **Attach**.
  - `home-lab`: stopped.
  - `studio-mini`: crash loop.
  - `pi-garden`: unreachable.
- **Tray**: the claw icon in the simulated OS menu bar shows the combined status and opens the tray menu. The red dot on the window closes it to the tray.
- **Prototype: simulate events** (bottom of the sidebar): fires the OS notifications decided in #10.
- **Deep links**: the URL hash is `#/<daemon>/<screen>[/<param>]`, e.g. `#/notes-assistant/job/weekly-review` or `#/studio-mini/status`. Add `?tray=1`, `?switcher=1` or `?notify=refused,crash,stopped,finished,update` to open those overlays.

## Screens

| Screen | Where |
|---|---|
| Status & Jobs | `src/screens/Status.tsx` (also the daemon-down, crash-loop and unreachable states) |
| Job editor: form, cron builder, next-run preview, launcher picker, raw markdown | `src/screens/JobEditor.tsx` |
| Chat with scope picker | `src/screens/Chat.tsx` |
| Sessions | `src/screens/Sessions.tsx` |
| Launchers + Test launcher | `src/screens/Launchers.tsx` |
| Settings (daemon + app) | `src/screens/Settings.tsx` |
| Logs (daemon, run logs, audit) | `src/screens/Logs.tsx` |
| Usage & cost | `src/screens/Usage.tsx` |
| Discord channels & threads (hire/fire) | `src/screens/Discord.tsx` |
| Agents | `src/screens/Agents.tsx` |

All names, hosts and IDs in `src/mock.ts` are made up. `192.0.2.x` is a documentation-only address range.
