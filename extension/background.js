// Runs in the background of Chrome. Whenever the active tab, its URL, or its video state changes,
// it sends {url, playing} to the widget at 127.0.0.1. When the widget answers "need_icon", it also
// sends that site's logo. Nothing leaves your laptop.

const BASE = 'http://127.0.0.1:47613';

// Per tab and frame: is media playing ('media') / is the mic in use ('mic').
// Kept in session storage because Chrome stops this background script when it's idle,
// which would otherwise wipe in-memory data.
async function getFlags(name) {
  const stored = await chrome.storage.session.get(name);
  return stored[name] || {};
}

// Each Chrome profile (work, fun...) runs its own copy of this extension. A random id per
// profile lets the widget tell them apart and use whichever one is actually focused.
async function profileId() {
  let { id } = await chrome.storage.local.get('id');
  if (!id) {
    id = crypto.randomUUID();
    await chrome.storage.local.set({ id });
  }
  return id;
}

// Same rule as the widget: "https://www.YouTube.com/x" -> "youtube.com".
function hostOf(url) {
  try {
    const u = new URL(url);
    if (u.protocol !== 'http:' && u.protocol !== 'https:') return null;
    return u.hostname.toLowerCase().replace(/^www\./, '') || null;
  } catch (_) {
    return null;
  }
}

// Send the site's favicon to the widget. It comes from Chrome's own favicon cache
// (the "favicon" permission), so nothing is downloaded from the internet.
async function sendIcon(tab) {
  const host = hostOf(tab?.url || '');
  if (!host || !tab.favIconUrl) return; // page hasn't got its real icon yet; we'll be asked again
  try {
    const src = new URL(chrome.runtime.getURL('/_favicon/'));
    src.searchParams.set('pageUrl', tab.url);
    src.searchParams.set('size', '32');
    const png = await (await fetch(src)).blob();
    await fetch(`${BASE}/icon`, {
      method: 'POST',
      headers: { 'Content-Type': 'image/png', 'X-STW': '1', 'X-Domain': host },
      body: png,
    });
  } catch (_) {
    // Widget not running; it will ask again next time.
  }
}

async function report() {
  let url = '';
  let playing = false;
  let focused = false;
  let tab = null;
  try {
    const win = await chrome.windows.getLastFocused({ populate: true });
    focused = !!win?.focused;
    tab = win?.tabs?.find((t) => t.active);
    if (tab) {
      url = tab.url || tab.pendingUrl || '';
      const frames = (await getFlags('media'))[tab.id] || {};
      playing = Object.values(frames).some(Boolean) || !!tab.audible;
    }
  } catch (_) {}
  // A tab in any window that's using the microphone (a Meet/Zoom-web call). The widget uses it
  // while Windows confirms Chrome holds the mic, so the call gets the time on any screen.
  let call_url = '';
  try {
    const mic = await getFlags('mic');
    for (const [tabId, frames] of Object.entries(mic)) {
      if (Object.values(frames).some(Boolean)) {
        call_url = (await chrome.tabs.get(Number(tabId))).url || '';
        break;
      }
    }
  } catch (_) {}
  try {
    const res = await fetch(`${BASE}/state`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json', 'X-STW': '1' },
      body: JSON.stringify({ id: await profileId(), url, playing, focused, call_url }),
    });
    // The widget says when it's missing this site's logo.
    if (tab && (await res.text()) === 'need_icon') sendIcon(tab);
  } catch (_) {
    // Widget isn't running. That's fine.
  }
}

// From content.js: {type:'media', playing} (video/audio) or {type:'mic', on} (call).
chrome.runtime.onMessage.addListener((msg, sender) => {
  if ((msg?.type !== 'media' && msg?.type !== 'mic') || !sender.tab) return;
  (async () => {
    const name = msg.type;
    const flags = await getFlags(name);
    const frames = (flags[sender.tab.id] ||= {});
    frames[sender.frameId ?? 0] = name === 'mic' ? !!msg.on : !!msg.playing;
    await chrome.storage.session.set({ [name]: flags });
    report();
  })();
});

chrome.tabs.onActivated.addListener(report);
chrome.tabs.onUpdated.addListener((_id, info) => {
  if (
    info.url !== undefined ||
    info.audible !== undefined ||
    info.favIconUrl !== undefined ||
    info.status === 'complete'
  ) {
    report();
  }
});
chrome.tabs.onRemoved.addListener(async (id) => {
  for (const name of ['media', 'mic']) {
    const flags = await getFlags(name);
    delete flags[id];
    await chrome.storage.session.set({ [name]: flags });
  }
  report();
});
chrome.windows.onFocusChanged.addListener(report);
chrome.runtime.onStartup.addListener(report);
chrome.runtime.onInstalled.addListener(report);

// Heartbeat every 30s, so the widget catches up if it was restarted.
chrome.alarms.create('heartbeat', { periodInMinutes: 0.5 });
chrome.alarms.onAlarm.addListener(report);
