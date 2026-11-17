// Dupli makes each sound from noise and sine waves. There are no audio files.
(() => {
'use strict';

let live = null;
let bus = null;
let enabled = true;
const noises = new WeakMap();
const lastAt = {};

function noiseFor(c) {
  let buf = noises.get(c);
  if (!buf) {
    buf = c.createBuffer(1, c.sampleRate, c.sampleRate);
    const d = buf.getChannelData(0);
    for (let i = 0; i < d.length; i++) d[i] = Math.random() * 2 - 1;
    noises.set(c, buf);
  }
  return buf;
}

const vary = (x, spread) => x * (1 + (Math.random() * 2 - 1) * spread);

function envelope(c, t, attack, dur, peak) {
  const g = c.createGain();
  g.gain.setValueAtTime(0.0001, t);
  g.gain.exponentialRampToValueAtTime(Math.max(peak, 0.0002), t + attack);
  g.gain.exponentialRampToValueAtTime(0.0001, t + dur);
  return g;
}

function click(c, dst, t, { f = 2400, q = 1.2, dur = 0.014, gain = 0.4 }) {
  const src = c.createBufferSource();
  src.buffer = noiseFor(c);
  const bp = c.createBiquadFilter();
  bp.type = 'bandpass';
  bp.frequency.value = f;
  bp.Q.value = q;
  src.connect(bp).connect(envelope(c, t, 0.0007, dur, gain)).connect(dst);
  src.start(t, Math.random() * 0.9, dur + 0.03);
}

function thump(c, dst, t, { f0 = 200, f1 = 110, dur = 0.05, gain = 0.4, type = 'sine' }) {
  const o = c.createOscillator();
  o.type = type;
  o.frequency.setValueAtTime(f0, t);
  o.frequency.exponentialRampToValueAtTime(f1, t + dur * 0.7);
  o.connect(envelope(c, t, 0.002, dur, gain)).connect(dst);
  o.start(t);
  o.stop(t + dur + 0.03);
}

function ring(c, dst, t, { f, dur, gain }) {
  const o = c.createOscillator();
  o.frequency.value = f;
  o.connect(envelope(c, t, 0.0015, dur, gain)).connect(dst);
  o.start(t);
  o.stop(t + dur + 0.03);
}

function hiss(c, dst, t, { f0, f1, dur, gain, q = 1 }) {
  const src = c.createBufferSource();
  src.buffer = noiseFor(c);
  const bp = c.createBiquadFilter();
  bp.type = 'bandpass';
  bp.Q.value = q;
  bp.frequency.setValueAtTime(f0, t);
  bp.frequency.linearRampToValueAtTime(f1, t + dur);
  const g = c.createGain();
  g.gain.setValueAtTime(0.0001, t);
  g.gain.linearRampToValueAtTime(gain, t + dur * 0.3);
  g.gain.linearRampToValueAtTime(0.0001, t + dur);
  src.connect(bp).connect(g).connect(dst);
  src.start(t, Math.random() * 0.9, dur + 0.03);
}

function buzz(c, dst, t, { f, dur, gain, cutoff }) {
  const o = c.createOscillator();
  o.type = 'sawtooth';
  o.frequency.value = f;
  const lp = c.createBiquadFilter();
  lp.type = 'lowpass';
  lp.frequency.value = cutoff;
  const g = c.createGain();
  g.gain.setValueAtTime(0.0001, t);
  g.gain.linearRampToValueAtTime(gain, t + 0.012);
  g.gain.setValueAtTime(gain, t + dur - 0.03);
  g.gain.linearRampToValueAtTime(0.0001, t + dur);
  o.connect(lp).connect(g).connect(dst);
  o.start(t);
  o.stop(t + dur + 0.03);
}

const VOICES = {
  keyDown(c, d, t, w = 0.5) {
    click(c, d, t, { f: vary(3000 - 1300 * w, 0.06), q: 1.4, dur: 0.012 + 0.008 * w, gain: 0.34 + 0.2 * w });
    click(c, d, t, { f: 6500, q: 0.8, dur: 0.004, gain: 0.1 });
    thump(c, d, t + 0.001, { f0: vary(270 - 90 * w, 0.05), f1: 150 - 50 * w, dur: 0.045 + 0.04 * w, gain: 0.26 + 0.24 * w, type: 'triangle' });
  },
  keyUp(c, d, t, w = 0.5) {
    click(c, d, t, { f: vary(3800 - 900 * w, 0.06), q: 1.6, dur: 0.008, gain: 0.16 + 0.08 * w });
    thump(c, d, t, { f0: 340 - 80 * w, f1: 230 - 60 * w, dur: 0.022, gain: 0.08 + 0.06 * w });
  },
  latch(c, d, t, on = true) {
    click(c, d, t + 0.03, { f: on ? 4300 : 2600, q: 2.2, dur: 0.007, gain: on ? 0.55 : 0.4 });
  },
  slide(c, d, t, steps = 1) {
    const run = 0.03 + 0.02 * steps;
    hiss(c, d, t, { f0: 1300, f1: 2300, dur: run, gain: 0.07, q: 0.9 });
    click(c, d, t + run, { f: vary(2900, 0.05), q: 1.8, dur: 0.009, gain: 0.3 });
    thump(c, d, t + run, { f0: 420, f1: 260, dur: 0.02, gain: 0.14 });
  },
  soft(c, d, t, w = 0) {
    thump(c, d, t, { f0: vary(560 - 140 * w, 0.04), f1: 380 - 100 * w, dur: 0.016 + 0.01 * w, gain: 0.2 + 0.08 * w, type: 'triangle' });
    click(c, d, t, { f: 2100, q: 1, dur: 0.005, gain: 0.16 });
  },
  queue(c, d, t) {
    click(c, d, t, { f: 1700, q: 1.6, dur: 0.012, gain: 0.3 });
    thump(c, d, t, { f0: 170, f1: 110, dur: 0.035, gain: 0.3 });
    click(c, d, t + 0.042, { f: 2500, q: 1.8, dur: 0.012, gain: 0.26 });
    thump(c, d, t + 0.042, { f0: 210, f1: 140, dur: 0.03, gain: 0.16 });
  },
  start(c, d, t) {
    thump(c, d, t, { f0: 150, f1: 82, dur: 0.11, gain: 0.5, type: 'triangle' });
    click(c, d, t, { f: 900, q: 0.9, dur: 0.028, gain: 0.3 });
    click(c, d, t + 0.006, { f: 3200, q: 1.5, dur: 0.008, gain: 0.14 });
  },
  tick(c, d, t) {
    click(c, d, t, { f: vary(4200, 0.08), q: 2.4, dur: 0.005, gain: 0.34 });
  },
  ready(c, d, t) {
    click(c, d, t, { f: 2800, q: 2, dur: 0.008, gain: 0.32 });
    click(c, d, t + 0.085, { f: 3300, q: 2, dur: 0.008, gain: 0.32 });
  },
  done(c, d, t, full = true) {
    click(c, d, t, { f: 1500, q: 1.4, dur: 0.014, gain: 0.3 });
    thump(c, d, t, { f0: 150, f1: 85, dur: 0.07, gain: 0.34 });
    const b = t + 0.09;
    const f = 1760;
    click(c, d, b, { f: 5200, q: 1.2, dur: 0.003, gain: 0.08 });
    ring(c, d, b, { f, dur: full ? 1.1 : 0.6, gain: full ? 0.16 : 0.1 });
    ring(c, d, b, { f: f * 2.006, dur: full ? 0.7 : 0.4, gain: full ? 0.07 : 0.04 });
    ring(c, d, b, { f: f * 3.01, dur: 0.3, gain: 0.025 });
  },
  attention(c, d, t) {
    for (const at of [0, 0.13]) {
      thump(c, d, t + at, { f0: 310, f1: 220, dur: 0.06, gain: 0.2, type: 'triangle' });
      click(c, d, t + at, { f: 1900, q: 1.5, dur: 0.008, gain: 0.2 });
    }
  },
  fault(c, d, t) {
    thump(c, d, t, { f0: 150, f1: 70, dur: 0.13, gain: 0.44, type: 'triangle' });
    buzz(c, d, t + 0.05, { f: 98, dur: 0.16, gain: 0.12, cutoff: 900 });
  },
  connect(c, d, t) {
    click(c, d, t, { f: 2300, q: 2, dur: 0.008, gain: 0.38 });
    thump(c, d, t, { f0: 280, f1: 190, dur: 0.02, gain: 0.18, type: 'triangle' });
  },
  disconnect(c, d, t) {
    click(c, d, t, { f: 1500, q: 1.6, dur: 0.01, gain: 0.3 });
    thump(c, d, t, { f0: 200, f1: 130, dur: 0.03, gain: 0.2, type: 'triangle' });
  },
};

const SPACING = { tick: 0.14, soft: 0.03, keyDown: 0.02, keyUp: 0.02 };

function output(c) {
  const comp = c.createDynamicsCompressor();
  comp.threshold.value = -14;
  comp.knee.value = 10;
  comp.ratio.value = 4;
  comp.attack.value = 0.002;
  comp.release.value = 0.08;
  const master = c.createGain();
  master.gain.value = 1;
  comp.connect(master).connect(c.destination);
  return comp;
}

function context() {
  if (live) return live;
  const AC = window.AudioContext || window.webkitAudioContext;
  if (!AC) return null;
  try {
    live = new AC({ latencyHint: 'interactive' });
    bus = output(live);
  } catch (e) {
    live = null;
  }
  return live;
}

window.DupliSound = {
  names: Object.keys(VOICES),
  set(on) { enabled = !!on; },
  get on() { return enabled; },
  wake() {
    const c = context();
    if (c && c.state === 'suspended') c.resume().catch(() => {});
  },
  play(name, ...args) {
    if (!enabled || !VOICES[name]) return;
    const c = context();
    if (!c) return;
    if (c.state === 'suspended') c.resume().catch(() => {});
    const now = c.currentTime;
    if (lastAt[name] != null && now - lastAt[name] < (SPACING[name] || 0.09)) return;
    lastAt[name] = now;
    VOICES[name](c, bus, now + 0.003, ...args);
  },
  render(name, args = [], seconds = 1.6) {
    const OC = window.OfflineAudioContext || window.webkitOfflineAudioContext;
    const c = new OC(1, Math.ceil(44100 * seconds), 44100);
    VOICES[name](c, output(c), 0.01, ...args);
    return c.startRendering().then((buf) => buf.getChannelData(0));
  },
};
})();
