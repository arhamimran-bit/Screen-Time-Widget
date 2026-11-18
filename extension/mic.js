// Runs inside the page itself ("MAIN world") so it can see when the page asks for the
// microphone, which is what Google Meet / Zoom web / Teams web do during a call.
// It only reports "mic on/off" (to content.js); it never reads or records any audio.
(() => {
  const md = navigator.mediaDevices;
  if (!md || !md.getUserMedia) return;

  const tracks = new Set();
  let last = false;
  const post = () => {
    for (const t of tracks) if (t.readyState !== 'live') tracks.delete(t);
    const on = tracks.size > 0;
    if (on !== last) {
      last = on;
      window.postMessage({ __stw: 'mic', on }, '*');
    }
  };

  const original = md.getUserMedia.bind(md);
  md.getUserMedia = async (constraints) => {
    const stream = await original(constraints);
    for (const t of stream.getAudioTracks()) {
      tracks.add(t);
      t.addEventListener('ended', post);
    }
    post();
    return stream;
  };

  // A page stopping the mic itself (track.stop()) fires no event, so re-check now and then.
  setInterval(post, 5000);
})();
