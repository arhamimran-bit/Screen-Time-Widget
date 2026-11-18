# How the Screen Time Widget works

A beginner-friendly tour of how a native Windows app works, comparing things to web development where it helps.

## The big picture

```
 ┌──────────────── Chrome ────────────────┐
 │ content.js  (in every page)            │
 │   "a video started/stopped"            │
 │        │                               │
 │        ▼                               │
 │ background.js                          │
 │   "active tab = youtube.com, playing"  │
 └────────┬───────────────────────────────┘
          │ HTTP POST to 127.0.0.1:47613 (never leaves the laptop)
          ▼
 ┌──────────── ScreenTimeWidget.exe ───────────────┐
 │ server.rs   (background thread)                 │
 │   stores the latest {url, playing}              │
 │                                                 │
 │ ui.rs       (main thread, the message loop)     │
 │   every 1s:  tracker.rs → "what's in front?"    │
 │              config.rs  → "which category?"     │
 │              storage.rs → add 1s to today       │
 │   on paint:  draw the widget                    │
 └─────────────────────────────────────────────────┘
          │
          ▼
 data\days\2026-11-17.json   (inside the project folder)
```

## 1. Every Windows app is a message loop

On the web, the browser runs an event loop and calls your `onclick` handlers.
A native Windows app does the same thing, but by hand:

```rust
while GetMessageW(&mut msg, ...) {   // wait for the next message
    DispatchMessageW(&msg);          // → calls our wndproc()
}
```

`wndproc` is one giant `match` over message types:

| Message | Web equivalent | What we do |
|---|---|---|
| `WM_PAINT` | render | Draw the whole widget |
| `WM_TIMER` | `setInterval` | Every 1s: track. Every 15ms during the slide animation: animate |
| `WM_LBUTTONDOWN/UP` | `mousedown/up` | Click a row, or start a drag |
| `WM_MOUSEMOVE` | `mousemove` | Hover highlight; if the button is held, hand the drag to Windows |
| `WM_RBUTTONUP` | `contextmenu` | Show the right-click menu |
| `WM_DESTROY` | `beforeunload` | Save and quit |

## 2. How tracking works (`tracker.rs`)

Windows has APIs for:
- `GetForegroundWindow()`: which window is in front
- `GetWindowThreadProcessId()` + `QueryFullProcessImageNameW()`: which `.exe` owns it (e.g. `Code.exe`)
- `GetLastInputInfo()`: when the keyboard or mouse was last touched (gives the idle time)

If the exe is `chrome.exe`, we use the website the extension last reported instead.

Then the **idle rule** decides whether this second counts:
```
counting = idle < idle_secs  OR  (video playing AND the rule allows video)
```
That's how YouTube keeps counting while you just watch, but a browser game can stop after 60 seconds with no input.

## 3. Why Chrome needs an extension

To Windows, every tab is just "chrome.exe". Only code inside Chrome knows the URL.
The extension is plain JS:
- `content.js` runs in each page and listens for `play` / `pause` on `<video>` elements.
- `background.js` watches tab switches and URL changes, and POSTs `{url, playing}` to the widget.

It's like calling your own API, except the "server" is a few lines in `server.rs`, listening only on your own laptop.

## 4. Drawing (`ui.rs`)

There's no HTML or CSS. We draw like on a `<canvas>`:
- **Direct2D**: rectangles, rounded rectangles, lines (like `ctx.fillRect`)
- **DirectWrite**: text (like `ctx.fillText`, but with proper fonts and ellipsis trimming)

Every paint redraws everything from scratch. It's a 460×214 image, so it takes under a millisecond.
The slide animation just draws the summary and the detail panel shifted by an x-offset that moves from 0 to 1 over 220ms.

## 5. Data and config

| File (in the project's `data\` folder) | What |
|---|---|
| `config.json` | Your categories, apps, sites and idle rules. Edit it and the widget reloads within ~3s. |
| `days/2026-11-17.json` | Seconds per app/site, plus seconds per hour (for the chart) |
| `settings.json` | Widget position, always-on-top, lock |

Categories aren't saved in the day files. They're worked out when drawing, so if you move a site from Other to Work, today's history moves with it.

## 6. Why it's so light

| | This widget | A Tauri app | An Electron app |
|---|---|---|---|
| Exe size | ~0.4 MB | ~5-10 MB | ~100+ MB |
| Own memory | ~7 MB | ~50-100 MB | ~150+ MB |

No browser engine, no runtime, no framework: just calls into Windows itself.
