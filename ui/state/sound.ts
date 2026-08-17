/**
 * Completion chime (Web Audio, no asset files). When an agent runtime flips out
 * of 运行中 the app plays a short confirmation sound and flashes the tab label.
 *
 * The chime is synthesized so it costs nothing to ship. Each theme maps to its
 * own `SoundConfig`; for now every theme shares the single confirmation sound
 * (confirmation-001) — add a `THEME_SOUNDS[themeId]` entry to give a theme its
 * own voice.
 */

export interface SoundConfig {
  /** Two notes (Hz) played in sequence — the confirmation "ding". */
  notes: [number, number];
  /** Note start offset in seconds, relative to the chime start. */
  starts: [number, number];
  /** Each note's duration in seconds. */
  durations: [number, number];
  /** Master gain envelope (0..1). */
  gain: number;
  waveform: OscillatorType;
}

/** Per-theme chime map. Missing ids fall back to the default confirmation
 *  sound; entries here give a theme its own voice. */
const THEME_SOUNDS: Record<string, SoundConfig> = {
  // 神秘蓝鲸 — slightly lower, longer pair (deep-water ping).
  "blue-whale": {
    notes: [523.25, 698.46],
    starts: [0, 0.11],
    durations: [0.16, 0.3],
    gain: 0.15,
    waveform: "sine",
  },
};

/** Default confirmation chime — confirmation-001. */
const CONFIRMATION_SOUND: SoundConfig = {
  notes: [659.25, 880],
  starts: [0, 0.09],
  durations: [0.14, 0.26],
  gain: 0.16,
  waveform: "sine",
};

export function soundForTheme(themeId: string): SoundConfig {
  return THEME_SOUNDS[themeId] ?? CONFIRMATION_SOUND;
}

let audioCtx: AudioContext | null = null;

/** Lazily-created, resumed-on-demand AudioContext (WebKitGTK keeps the context
 *  suspended until a user gesture, so every play attempt resumes it). */
function audioContext(): AudioContext | null {
  if (typeof window === "undefined") return null;
  const Ctor =
    window.AudioContext ??
    (window as unknown as { webkitAudioContext?: typeof AudioContext }).webkitAudioContext;
  if (!Ctor) return null;
  if (!audioCtx) {
    try {
      audioCtx = new Ctor();
    } catch {
      return null;
    }
  }
  if (audioCtx.state === "suspended") void audioCtx.resume();
  return audioCtx;
}

/** Play the theme-mapped confirmation chime. Safe no-op when Web Audio is
 *  unavailable. */
export function playConfirmationSound(themeId: string): void {
  const ac = audioContext();
  if (!ac) return;
  const { notes, starts, durations, gain, waveform } = soundForTheme(themeId);
  const t0 = ac.currentTime;
  const master = ac.createGain();
  master.gain.value = gain;
  master.connect(ac.destination);
  notes.forEach((freq, i) => {
    const osc = ac.createOscillator();
    osc.type = waveform;
    osc.frequency.value = freq;
    const env = ac.createGain();
    // Short attack + exponential decay avoids a click at note boundaries.
    env.gain.setValueAtTime(0, t0 + starts[i]);
    env.gain.linearRampToValueAtTime(1, t0 + starts[i] + 0.008);
    env.gain.exponentialRampToValueAtTime(0.001, t0 + starts[i] + durations[i]);
    osc.connect(env);
    env.connect(master);
    osc.start(t0 + starts[i]);
    osc.stop(t0 + starts[i] + durations[i] + 0.02);
  });
}