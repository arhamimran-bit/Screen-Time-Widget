// Runs inside every web page. Its only jobs: notice when a <video>/<audio> starts or stops
// playing, and relay mic on/off from mic.js, and tell background.js.
// It never reads or sends page content.
(() => {
  let last = null;

  const isPlaying = () =>
    [...document.querySelectorAll('video, audio')].some((m) => !m.paused && !m.ended);

  const send = (msg) => {
    try {
      chrome.runtime.sendMessage(msg).catch(() => {});
    } catch (_) {
      // Extension was reloaded; this old script can't talk to it anymore.
    }
  };

  const check = () => {
    const playing = isPlaying();
    if (playing !== last) {
      last = playing;
      send({ type: 'media', playing });
    }
  };

  // Media events don't bubble, but listening in the capture phase on document catches them all.
  for (const ev of ['play', 'playing', 'pause', 'ended', 'emptied']) {
    document.addEventListener(ev, check, true);
  }

  // Microphone on/off, from mic.js running in the page.
  let micOn = false;
  window.addEventListener('message', (e) => {
    if (e.source === window && e.data?.__stw === 'mic') {
      micOn = !!e.data.on;
      send({ type: 'mic', on: micOn });
    }
  });

  addEventListener('pagehide', () => {
    if (last) {
      last = false;
      send({ type: 'media', playing: false });
    }
    if (micOn) {
      micOn = false;
      send({ type: 'mic', on: false });
    }
  });
})();
