// The social cut of a demo tape: its VHS recording in a neon window on an
// animated background, cut to the music: the title builds on the track's
// rising hits and the window slams in on its drop, the camera punches onto
// where the tape's marks say, with shakes, flashes, glitching captions, key
// caps and an end card. The marks and their times come from
// public/<name>.json, which scripts/demo/tape.py writes, so they are data,
// not editable in Studio; the text, look and music are props.
import { Audio, Video } from "@remotion/media";
import { loadFont as loadGeist } from "@remotion/google-fonts/Geist";
import { loadFont as loadGeistMono } from "@remotion/google-fonts/GeistMono";
import React from "react";
import {
  AbsoluteFill,
  CalculateMetadataFunction,
  Freeze,
  interpolate,
  Sequence,
  spring,
  staticFile,
  useCurrentFrame,
  useVideoConfig,
} from "remotion";

const { fontFamily: SANS } = loadGeist("normal", {
  weights: ["500", "700", "800", "900"],
  subsets: ["latin"],
});
const { fontFamily: MONO } = loadGeistMono("normal", {
  weights: ["500", "700"],
  subsets: ["latin"],
});

// Horizon, the recording's theme
const BG = "#1c1e26";
const PINK = "#e95678";
const ORANGE = "#fab795";
const CYAN = "#26bbd9";
const TEXT = "#d5d8da";

type Rect = [number, number, number, number];
type Segment = { from: number; to: number; rate: number; at: number };
export type Timeline = {
  segments: Segment[];
  captions: { at: number; text: string }[];
  keys: { at: number; text: string }[];
  focus: { at: number; rect: Rect | null }[];
  highlights: { at: number; rect: Rect }[];
  duration: number;
};

export type DemoProps = {
  name: string;
  windowTitle: string;
  headline: string;
  subline: string;
  alsoIn: string;
  music: string | null;
  musicVolume: number;
  // Seconds into the track of its drop, which the window slams in on, and
  // its tempo, which the background and window pulse to
  musicDrop: number;
  bpm: number;
  timeline: Timeline | null;
};

const TITLE = 4.5; // seconds of title, ending on the drop: 12 beats at 160 BPM
const END = 3.6; // seconds of end card after the recording
const REC_W = 1920;
const REC_H = 996;
const WIN_W = 1600;
const WIN_H = (WIN_W * REC_H) / REC_W;
const BAR_H = 44;
const WIN_X = (1920 - WIN_W) / 2;
const WIN_Y = 36;

export const calculateDemoMetadata: CalculateMetadataFunction<
  DemoProps
> = async ({ props }) => {
  const res = await fetch(staticFile(`${props.name}.json`));
  const recorded = (await res.json()) as Timeline;
  // The end card lands on a downbeat: the recording's part is held on its
  // last frame up to the next bar
  const bar = (4 * 60) / props.bpm;
  const timeline = {
    ...recorded,
    duration: Math.ceil(recorded.duration / bar - 0.1) * bar,
  };
  const fps = 30;
  return {
    fps,
    durationInFrames: Math.ceil((TITLE + timeline.duration + END) * fps),
    props: { ...props, timeline },
  };
};

// How hard the beat hits at a frame: 1 on each beat, decaying before the
// next; `every` beats apart for bars
function kick(frame: number, fps: number, bpm: number, every = 1): number {
  const len = (60 / bpm) * every;
  const t = frame / fps - TITLE; // the drop is a downbeat
  const phase = (((t / len) % 1) + 1) % 1;
  return Math.exp(-phase * 7);
}

// A decaying shake, `since` seconds after it started
function shake(since: number, amp: number): [number, number] {
  if (since < 0 || since > 0.4) return [0, 0];
  const a = amp * (1 - since / 0.4);
  return [a * Math.sin(since * 95), a * Math.cos(since * 77)];
}

const clamp = { extrapolateLeft: "clamp", extrapolateRight: "clamp" } as const;

export const Demo: React.FC<DemoProps> = (props) => {
  const { fps } = useVideoConfig();
  const timeline = props.timeline;
  if (!timeline) return null;
  const start = Math.round(TITLE * fps);
  const contentFrames = Math.round(timeline.duration * fps);
  return (
    <AbsoluteFill style={{ backgroundColor: BG, fontFamily: SANS }}>
      <Background bpm={props.bpm} />
      <Sequence from={start} name="Window">
        <Stage
          timeline={timeline}
          name={props.name}
          windowTitle={props.windowTitle}
          contentFrames={contentFrames}
          bpm={props.bpm}
        />
      </Sequence>
      <Sequence durationInFrames={start} name="Title">
        <TitleCard
          headline={props.headline}
          subline={props.subline}
          bpm={props.bpm}
        />
      </Sequence>
      <Sequence from={start} durationInFrames={contentFrames} name="Captions">
        <Captions captions={timeline.captions} />
        <Keys keys={timeline.keys} />
      </Sequence>
      <Sequence from={start + contentFrames} name="End card">
        <EndCard alsoIn={props.alsoIn} />
      </Sequence>
      <Sequence from={start} durationInFrames={10} name="Drop flash">
        <Flash strength={0.75} />
      </Sequence>
      <Sequence
        from={start + contentFrames}
        durationInFrames={10}
        name="End flash"
      >
        <Flash strength={0.45} />
      </Sequence>
      <Scanlines />
      {props.music ? (
        <MusicTrack
          src={props.music}
          volume={props.musicVolume}
          from={props.musicDrop - TITLE}
        />
      ) : null}
    </AbsoluteFill>
  );
};

const MusicTrack: React.FC<{ src: string; volume: number; from: number }> = ({
  src,
  volume,
  from,
}) => {
  const { fps, durationInFrames } = useVideoConfig();
  return (
    <Audio
      src={staticFile(src)}
      trimBefore={Math.round(from * fps)}
      volume={(f) =>
        volume *
        interpolate(
          f,
          [0, 0.3 * fps, durationInFrames - 2.5 * fps, durationInFrames],
          [0, 1, 1, 0],
          clamp,
        )
      }
    />
  );
};

const Flash: React.FC<{ strength: number }> = ({ strength }) => {
  const frame = useCurrentFrame();
  return (
    <AbsoluteFill
      style={{
        backgroundColor: "white",
        opacity: interpolate(frame, [0, 9], [strength, 0], clamp),
        mixBlendMode: "screen",
      }}
    />
  );
};

const Scanlines: React.FC = () => (
  <AbsoluteFill
    style={{
      pointerEvents: "none",
      backgroundImage:
        "repeating-linear-gradient(0deg, rgba(0,0,0,0.10) 0px, rgba(0,0,0,0.10) 1px, transparent 1px, transparent 4px)",
      boxShadow: "inset 0 0 220px rgba(0,0,0,0.65)",
    }}
  />
);

// Three blurred blobs of the theme's colours, drifting, brightening on the
// beat, over a neon grid
const Background: React.FC<{ bpm: number }> = ({ bpm }) => {
  const frame = useCurrentFrame();
  const { fps } = useVideoConfig();
  const k = kick(frame, fps, bpm);
  const blob = (color: string, x: number, y: number, phase: number) => (
    <div
      style={{
        position: "absolute",
        width: 1100,
        height: 1100,
        borderRadius: "50%",
        background: `radial-gradient(circle, ${color} 0%, transparent 65%)`,
        opacity: 0.26 + 0.16 * k,
        left: x + Math.sin(frame / 90 + phase) * 160,
        top: y + Math.cos(frame / 110 + phase) * 120,
        filter: "blur(40px)",
      }}
    />
  );
  return (
    <AbsoluteFill style={{ overflow: "hidden" }}>
      <AbsoluteFill
        style={{
          backgroundImage: `linear-gradient(${PINK}22 1px, transparent 1px), linear-gradient(90deg, ${PINK}22 1px, transparent 1px)`,
          backgroundSize: "80px 80px",
          backgroundPosition: `0px ${(frame * 2) % 80}px`,
          opacity: 0.5 + 0.5 * k,
        }}
      />
      {blob(PINK, -300, -400, 0)}
      {blob(CYAN, 1200, 100, 2)}
      {blob(ORANGE, 300, 500, 4)}
    </AbsoluteFill>
  );
};

// Text split into red and cyan copies that snap together over `frames`
function glitch(since: number, frames: number, amp: number) {
  const d = since < 0 ? amp : amp * Math.max(0, 1 - since / frames);
  const jitter = since < frames ? Math.sin(since * 13) * d * 0.6 : 0;
  return {
    textShadow: d
      ? `${d}px 0 ${PINK}, ${-d}px 0 ${CYAN}`
      : "0 0 30px rgba(0,0,0,0.4)",
    translate: `${jitter}px 0px`,
  };
}

// The title's lines land on the build's beats, early enough to read: the
// label twelve beats before the drop, the headline eight, the subline six;
// the build's hits four and two beats before it bump the title and glitch
// the headline
const TitleCard: React.FC<{
  headline: string;
  subline: string;
  bpm: number;
}> = ({ headline, subline, bpm }) => {
  const frame = useCurrentFrame();
  const { fps } = useVideoConfig();
  const beat = (60 / bpm) * fps;
  const at = (beats: number) => TITLE * fps - beats * beat;
  const slam = (from: number) =>
    spring({
      frame: frame - from,
      fps,
      config: { damping: 13, stiffness: 320, mass: 0.6 },
    });
  const label = at(12) + 2;
  const head = at(8);
  const sub = at(6);
  const hits = [at(4), at(2)];
  let hit = 0;
  for (const h of hits) {
    if (frame >= h) hit = Math.max(hit, Math.exp(-(frame - h) / 3));
  }
  const lastHit = Math.max(...hits.filter((h) => frame >= h), -1000);
  return (
    <AbsoluteFill
      style={{
        justifyContent: "center",
        alignItems: "center",
        flexDirection: "column",
        gap: 24,
        backgroundColor: "rgba(16,17,22,0.55)",
        scale: String(1 + 0.05 * hit),
      }}
    >
      <div
        style={{
          fontFamily: MONO,
          fontSize: 42,
          color: ORANGE,
          letterSpacing: 8,
          opacity: frame >= label ? 1 : 0,
          ...glitch(frame - label, 6, 10),
        }}
      >
        NEOPANE
      </div>
      <div
        style={{
          fontSize: 150,
          fontWeight: 900,
          color: "white",
          letterSpacing: -4,
          opacity: frame >= head ? 1 : 0,
          scale: String(1.7 - 0.7 * slam(head)),
          ...glitch(Math.min(frame - head, frame - lastHit), 7, 14),
        }}
      >
        {headline}
      </div>
      <div
        style={{
          fontSize: 80,
          fontWeight: 800,
          letterSpacing: -1.5,
          backgroundImage: `linear-gradient(90deg, ${PINK}, ${ORANGE})`,
          backgroundClip: "text",
          WebkitBackgroundClip: "text",
          color: "transparent",
          opacity: frame >= sub ? 1 : 0,
          scale: String(1.4 - 0.4 * slam(sub)),
        }}
      >
        {subline}
      </div>
    </AbsoluteFill>
  );
};

// The window with the recording, its camera and its highlights. It slams
// in on the drop, shakes as panes appear and the camera moves, and its
// neon edge pulses with the beat.
const Stage: React.FC<{
  timeline: Timeline;
  name: string;
  windowTitle: string;
  contentFrames: number;
  bpm: number;
}> = ({ timeline, name, windowTitle, contentFrames, bpm }) => {
  const frame = useCurrentFrame();
  const { fps } = useVideoConfig();
  const enter = spring({
    frame,
    fps,
    config: { damping: 14, stiffness: 260, mass: 0.7 },
  });
  const exit = spring({
    frame: frame - contentFrames,
    fps,
    config: { damping: 16, stiffness: 200 },
  });
  const t = frame / fps;
  const cam = camera(timeline.focus, t, fps);
  const k = kick(frame + TITLE * fps, fps, bpm);
  const hits = [
    0,
    ...timeline.highlights.map((h) => h.at + 0.35),
    ...timeline.focus.map((f) => f.at),
  ];
  let sx = 0;
  let sy = 0;
  for (const h of hits) {
    const [x, y] = shake(t - h, h === 0 ? 14 : 6);
    sx += x;
    sy += y;
  }
  const last = timeline.segments[timeline.segments.length - 1];
  return (
    <AbsoluteFill>
      <div
        style={{
          position: "absolute",
          left: WIN_X,
          top: WIN_Y,
          width: WIN_W,
          height: WIN_H + BAR_H,
          borderRadius: 16,
          overflow: "hidden",
          backgroundColor: BG,
          border: `1.5px solid rgba(233,86,120,${0.35 + 0.45 * k})`,
          boxShadow: `0 50px 140px rgba(0,0,0,0.7), 0 0 ${30 + 50 * k}px rgba(233,86,120,${0.25 + 0.35 * k})`,
          translate: `${sx}px ${sy - 330 * exit}px`,
          scale: String(1.18 - 0.18 * enter - 0.42 * exit),
          opacity: Math.min(1, frame / 3) * (1 - 0.25 * exit),
        }}
      >
        <TitleBar title={windowTitle} />
        <div
          style={{
            position: "relative",
            width: WIN_W,
            height: WIN_H,
            overflow: "hidden",
          }}
        >
          <div
            style={{
              position: "absolute",
              width: WIN_W,
              height: WIN_H,
              transformOrigin: "0 0",
              translate: `${cam.x}px ${cam.y}px`,
              scale: String(cam.s),
            }}
          >
            {timeline.segments.map((s, i) => (
              <Sequence
                key={i}
                from={Math.round(s.at * fps)}
                durationInFrames={Math.max(
                  1,
                  Math.round(((s.to - s.from) / s.rate) * fps),
                )}
                name={`Recording ${i + 1}`}
              >
                <Video
                  src={staticFile(`${name}.mp4`)}
                  trimBefore={Math.round(s.from * fps)}
                  playbackRate={s.rate}
                  muted
                  style={{ width: WIN_W, height: WIN_H }}
                />
              </Sequence>
            ))}
            <Sequence
              from={Math.round(
                (last.at + (last.to - last.from) / last.rate) * fps,
              )}
              name="Last frame, held"
            >
              <Freeze frame={0}>
                <Video
                  src={staticFile(`${name}.mp4`)}
                  trimBefore={Math.round((last.to - 0.1) * fps)}
                  muted
                  style={{ width: WIN_W, height: WIN_H }}
                />
              </Freeze>
            </Sequence>
            {timeline.highlights.map((h, i) => (
              <Sequence
                key={i}
                from={Math.round((h.at + 0.35) * fps)}
                durationInFrames={Math.round(1.8 * fps)}
                name={`Highlight ${i + 1}`}
              >
                <Highlight rect={h.rect} />
              </Sequence>
            ))}
          </div>
        </div>
      </div>
    </AbsoluteFill>
  );
};

// Where the camera is at t seconds into the recording: a punchy spring
// from the last focus to the current one, each scaled to fill the window
// with its rectangle, at most 1.9 times, and kept inside the recording.
function camera(
  focus: Timeline["focus"],
  t: number,
  fps: number,
): { s: number; x: number; y: number } {
  const state = (rect: Rect | null) => {
    if (!rect) return { s: 1, cx: 0.5, cy: 0.5 };
    const [x, y, w, h] = rect;
    const s = Math.min(1.9, Math.max(1, 0.94 * Math.min(1 / w, 1 / h)));
    return { s, cx: x + w / 2, cy: y + h / 2 };
  };
  let prev = state(null);
  let cur = prev;
  let since = Infinity;
  for (const f of focus) {
    if (f.at > t) break;
    prev = cur;
    cur = state(f.rect);
    since = t - f.at;
  }
  const p =
    since === Infinity
      ? 1
      : spring({
          frame: since * fps,
          fps,
          config: { damping: 16, stiffness: 190, mass: 0.7 },
        });
  const s = prev.s + (cur.s - prev.s) * p;
  const cx = prev.cx + (cur.cx - prev.cx) * p;
  const cy = prev.cy + (cur.cy - prev.cy) * p;
  const lim = (v: number, lo: number, hi: number) =>
    Math.min(hi, Math.max(lo, v));
  return {
    s,
    x: lim(WIN_W / 2 - cx * WIN_W * s, WIN_W - WIN_W * s, 0),
    y: lim(WIN_H / 2 - cy * WIN_H * s, WIN_H - WIN_H * s, 0),
  };
}

const TitleBar: React.FC<{ title: string }> = ({ title }) => (
  <div
    style={{
      height: BAR_H,
      display: "flex",
      alignItems: "center",
      justifyContent: "center",
      position: "relative",
      backgroundColor: "#232530",
      borderBottom: "1px solid rgba(255,255,255,0.06)",
    }}
  >
    <div style={{ position: "absolute", left: 18, display: "flex", gap: 9 }}>
      {["#ff5f57", "#febc2e", "#28c840"].map((c) => (
        <div
          key={c}
          style={{
            width: 14,
            height: 14,
            borderRadius: 7,
            backgroundColor: c,
          }}
        />
      ))}
    </div>
    <div style={{ fontSize: 19, fontWeight: 500, color: "#9da0a8" }}>
      {title}
    </div>
  </div>
);

const Highlight: React.FC<{ rect: Rect }> = ({ rect }) => {
  const frame = useCurrentFrame();
  const { fps, durationInFrames } = useVideoConfig();
  const [x, y, w, h] = rect;
  const opacity = interpolate(
    frame,
    [0, 3, durationInFrames - 0.4 * fps, durationInFrames],
    [0, 1, 1, 0],
    clamp,
  );
  const burst = interpolate(frame, [0, 10], [1, 0], clamp);
  const grow = spring({
    frame,
    fps,
    config: { damping: 11, stiffness: 260 },
  });
  return (
    <div
      style={{
        position: "absolute",
        left: x * WIN_W,
        top: y * WIN_H,
        width: w * WIN_W,
        height: h * WIN_H,
        borderRadius: 12,
        border: `4px solid ${ORANGE}`,
        boxShadow: `0 0 ${36 + 60 * burst}px ${ORANGE}aa, inset 0 0 ${36 + 80 * burst}px ${ORANGE}${burst > 0.5 ? "66" : "33"}`,
        backgroundColor: `rgba(250,183,149,${0.12 * burst})`,
        opacity,
        scale: String(1.08 - 0.08 * grow),
      }}
    />
  );
};

// `text` in backticks is set in the monospace face, in the accent colour
const Rich: React.FC<{ text: string }> = ({ text }) => (
  <>
    {text.split("`").map((part, i) =>
      i % 2 ? (
        <span key={i} style={{ fontFamily: MONO, color: ORANGE }}>
          {part}
        </span>
      ) : (
        <span key={i}>{part}</span>
      ),
    )}
  </>
);

const Captions: React.FC<{ captions: Timeline["captions"] }> = ({
  captions,
}) => {
  const frame = useCurrentFrame();
  const { fps } = useVideoConfig();
  const t = frame / fps;
  let i = -1;
  captions.forEach((c, j) => {
    if (c.at <= t) i = j;
  });
  if (i < 0 || !captions[i].text) return null;
  const since = (t - captions[i].at) * fps;
  const pop = spring({
    frame: since,
    fps,
    config: { damping: 13, stiffness: 300, mass: 0.5 },
  });
  return (
    <AbsoluteFill style={{ justifyContent: "flex-end", alignItems: "center" }}>
      <div
        style={{
          marginBottom: 34,
          padding: "16px 36px",
          borderRadius: 20,
          backgroundColor: "rgba(14,15,20,0.88)",
          border: `1px solid ${PINK}66`,
          boxShadow: `0 20px 60px rgba(0,0,0,0.6), 0 0 30px ${PINK}33`,
          fontSize: 48,
          fontWeight: 800,
          color: "white",
          letterSpacing: -0.5,
          whiteSpace: "nowrap",
          opacity: Math.min(1, pop * 2),
          scale: String(1.15 - 0.15 * pop),
          ...glitch(since, 6, 9),
        }}
      >
        <Rich text={captions[i].text} />
      </div>
    </AbsoluteFill>
  );
};

const Keys: React.FC<{ keys: Timeline["keys"] }> = ({ keys }) => {
  const frame = useCurrentFrame();
  const { fps } = useVideoConfig();
  const t = frame / fps;
  const shown = keys.filter((k) => k.at <= t && t < k.at + 1.9);
  if (!shown.length) return null;
  const k = shown[shown.length - 1];
  const since = (t - k.at) * fps;
  const out = interpolate(since, [1.6 * fps, 1.9 * fps], [1, 0], clamp);
  return (
    <div
      style={{
        position: "absolute",
        right: WIN_X + 36,
        top: WIN_Y + BAR_H + WIN_H - 150,
        display: "flex",
        gap: 14,
        opacity: out,
      }}
    >
      {k.text.split(" ").map((key, i) => {
        const pop = spring({
          frame: since - i * 3,
          fps,
          config: { damping: 9, stiffness: 300, mass: 0.5 },
        });
        const pressed = interpolate(since - i * 3, [3, 5, 8], [0, 1, 0], clamp);
        return (
          <div
            key={i}
            style={{
              minWidth: 70,
              padding: "12px 22px",
              borderRadius: 14,
              background: "linear-gradient(180deg, #343747, #262833)",
              border: `1px solid ${pressed > 0.3 ? CYAN : "#50546a"}`,
              borderBottom: "6px solid #14151b",
              boxShadow: `0 16px 40px rgba(0,0,0,0.55), 0 0 ${30 + 40 * pressed}px ${CYAN}${pressed > 0.3 ? "99" : "44"}`,
              fontFamily: MONO,
              fontSize: 40,
              fontWeight: 700,
              color: "white",
              textAlign: "center",
              opacity: Math.min(1, pop * 1.5),
              scale: String(0.5 + 0.5 * pop),
              translate: `0px ${30 * (1 - pop) + 5 * pressed}px`,
            }}
          >
            {key}
          </div>
        );
      })}
    </div>
  );
};

const EndCard: React.FC<{ alsoIn: string }> = ({ alsoIn }) => {
  const frame = useCurrentFrame();
  const { fps } = useVideoConfig();
  const slam = (delay: number) =>
    spring({
      frame: frame - delay,
      fps,
      config: { damping: 13, stiffness: 300, mass: 0.6 },
    });
  return (
    <AbsoluteFill
      style={{
        justifyContent: "flex-end",
        alignItems: "center",
        flexDirection: "column",
        paddingBottom: 110,
        gap: 26,
      }}
    >
      <div
        style={{
          fontFamily: MONO,
          fontSize: 130,
          fontWeight: 700,
          letterSpacing: -4,
          backgroundImage: `linear-gradient(90deg, ${PINK}, ${ORANGE})`,
          backgroundClip: "text",
          WebkitBackgroundClip: "text",
          color: "transparent",
          scale: String(1.6 - 0.6 * slam(2)),
          opacity: frame >= 2 ? 1 : 0,
          filter: `drop-shadow(0 0 30px ${PINK}88)`,
        }}
      >
        neopane
      </div>
      <div
        style={{
          fontFamily: MONO,
          fontSize: 38,
          color: "white",
          padding: "16px 32px",
          borderRadius: 16,
          backgroundColor: "rgba(14,15,20,0.88)",
          border: `1px solid ${CYAN}66`,
          boxShadow: `0 0 30px ${CYAN}33`,
          opacity: frame >= 8 ? 1 : 0,
          scale: String(1.25 - 0.25 * slam(8)),
        }}
      >
        <span style={{ color: CYAN }}>$</span> brew install
        spiritledsoftware/tap/neopane
      </div>
      <div
        style={{
          fontSize: 36,
          fontWeight: 500,
          color: TEXT,
          opacity: interpolate(frame, [14, 22], [0, 1], clamp),
        }}
      >
        github.com/ian-pascoe/neopane
        {alsoIn ? <span style={{ color: "#8a8d99" }}> · {alsoIn}</span> : null}
      </div>
    </AbsoluteFill>
  );
};
