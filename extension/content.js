// Runs inside every web page. Its only job: notice when a <video>/<audio> starts or stops
// playing, and tell background.js. It never reads or sends page content.
(() => {
  let last = null;

  const isPlaying = () =>
    [...document.querySelectorAll('video, audio')].some((m) => !m.paused && !m.ended);

  const send = (playing) => {
    try {
      chrome.runtime.sendMessage({ type: 'media', playing }).catch(() => {});
    } catch (_) {
      // Extension was reloaded; this old script can't talk to it anymore.
    }
  };

  const check = () => {
    const playing = isPlaying();
    if (playing !== last) {
      last = playing;
      send(playing);
    }
  };

  // Media events don't bubble, but listening in the capture phase on document catches them all.
  for (const ev of ['play', 'playing', 'pause', 'ended', 'emptied']) {
    document.addEventListener(ev, check, true);
  }
  addEventListener('pagehide', () => {
    if (last) {
      last = false;
      send(false);
    }
  });
})();
