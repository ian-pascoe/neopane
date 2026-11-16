// The social cut of a demo tape: its VHS recording in a window on an animated
// background, zooming and outlining where the tape's marks say, with big
// captions, key caps, a title, an end card and music. The marks and their
// times come from public/<name>.json, which scripts/demo/tape.py writes, so
// they are data, not editable in Studio; the text and look are props.
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
  weights: ["500", "700", "800"],
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
  timeline: Timeline | null;
};

const TITLE = 2.2; // seconds before the recording starts
const END = 3.4; // seconds of end card after it
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
  const timeline = (await res.json()) as Timeline;
  const fps = 30;
  return {
    fps,
    durationInFrames: Math.ceil((TITLE + timeline.duration + END) * fps),
    props: { ...props, timeline },
  };
};

export const Demo: React.FC<DemoProps> = (props) => {
  const { fps } = useVideoConfig();
  const timeline = props.timeline;
  if (!timeline) return null;
  const start = Math.round(TITLE * fps);
  const contentFrames = Math.round(timeline.duration * fps);
  return (
    <AbsoluteFill style={{ backgroundColor: BG, fontFamily: SANS }}>
      <Background />
      <Sequence from={start - Math.round(0.5 * fps)} name="Window">
        <Stage
          timeline={timeline}
          name={props.name}
          windowTitle={props.windowTitle}
          lead={Math.round(0.5 * fps)}
          contentFrames={contentFrames}
        />
      </Sequence>
      <Sequence durationInFrames={start + Math.round(0.3 * fps)} name="Title">
        <TitleCard headline={props.headline} subline={props.subline} />
      </Sequence>
      <Sequence from={start} durationInFrames={contentFrames} name="Captions">
        <Captions captions={timeline.captions} />
        <Keys keys={timeline.keys} />
      </Sequence>
      <Sequence from={start + contentFrames} name="End card">
        <EndCard alsoIn={props.alsoIn} />
      </Sequence>
      {props.music ? (
        <MusicTrack src={props.music} volume={props.musicVolume} />
      ) : null}
    </AbsoluteFill>
  );
};

const MusicTrack: React.FC<{ src: string; volume: number }> = ({
  src,
  volume,
}) => {
  const { fps, durationInFrames } = useVideoConfig();
  return (
    <Audio
      src={staticFile(src)}
      volume={(f) =>
        volume *
        interpolate(
          f,
          [0, fps, durationInFrames - 2 * fps, durationInFrames],
          [0, 1, 1, 0],
          { extrapolateLeft: "clamp", extrapolateRight: "clamp" },
        )
      }
    />
  );
};

// Three slow, blurred blobs of the theme's colours
const Background: React.FC = () => {
  const frame = useCurrentFrame();
  const blob = (color: string, x: number, y: number, phase: number) => (
    <div
      style={{
        position: "absolute",
        width: 1100,
        height: 1100,
        borderRadius: "50%",
        background: `radial-gradient(circle, ${color} 0%, transparent 65%)`,
        opacity: 0.32,
        left: x + Math.sin(frame / 90 + phase) * 160,
        top: y + Math.cos(frame / 110 + phase) * 120,
        filter: "blur(40px)",
      }}
    />
  );
  return (
    <AbsoluteFill style={{ overflow: "hidden" }}>
      {blob(PINK, -300, -400, 0)}
      {blob(CYAN, 1200, 100, 2)}
      {blob(ORANGE, 300, 500, 4)}
    </AbsoluteFill>
  );
};

const TitleCard: React.FC<{ headline: string; subline: string }> = ({
  headline,
  subline,
}) => {
  const frame = useCurrentFrame();
  const { fps, durationInFrames } = useVideoConfig();
  const pop = (delay: number) =>
    spring({ frame: frame - delay, fps, config: { damping: 14, mass: 0.6 } });
  const out = interpolate(
    frame,
    [durationInFrames - 0.6 * fps, durationInFrames],
    [0, 1],
    { extrapolateLeft: "clamp", extrapolateRight: "clamp" },
  );
  return (
    <AbsoluteFill
      style={{
        justifyContent: "center",
        alignItems: "center",
        flexDirection: "column",
        gap: 28,
        opacity: 1 - out,
        translate: `0px ${-80 * out}px`,
        backgroundColor: `rgba(28,30,38,${0.85 * (1 - out)})`,
      }}
    >
      <div
        style={{
          fontFamily: MONO,
          fontSize: 40,
          color: ORANGE,
          letterSpacing: 2,
          opacity: pop(0),
        }}
      >
        neopane
      </div>
      <div
        style={{
          fontSize: 132,
          fontWeight: 800,
          color: "white",
          letterSpacing: -3,
          scale: String(0.8 + 0.2 * pop(4)),
          opacity: pop(4),
        }}
      >
        {headline}
      </div>
      <div
        style={{
          fontSize: 76,
          fontWeight: 700,
          letterSpacing: -1.5,
          backgroundImage: `linear-gradient(90deg, ${PINK}, ${ORANGE})`,
          backgroundClip: "text",
          WebkitBackgroundClip: "text",
          color: "transparent",
          translate: `0px ${40 * (1 - pop(14))}px`,
          opacity: pop(14),
        }}
      >
        {subline}
      </div>
    </AbsoluteFill>
  );
};

// The window with the recording, its camera and its highlights. Its frame
// 0 is `lead` frames before the recording starts, for its entrance.
const Stage: React.FC<{
  timeline: Timeline;
  name: string;
  windowTitle: string;
  lead: number;
  contentFrames: number;
}> = ({ timeline, name, windowTitle, lead, contentFrames }) => {
  const frame = useCurrentFrame();
  const { fps } = useVideoConfig();
  const enter = spring({ frame, fps, config: { damping: 18, mass: 0.8 } });
  const exit = spring({
    frame: frame - lead - contentFrames,
    fps,
    config: { damping: 20 },
  });
  const t = (frame - lead) / fps;
  const cam = camera(timeline.focus, t, fps);
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
          border: "1px solid rgba(255,255,255,0.12)",
          boxShadow:
            "0 50px 140px rgba(0,0,0,0.65), 0 0 0 1px rgba(0,0,0,0.4)",
          translate: `0px ${interpolate(enter, [0, 1], [700, 0]) - 330 * exit}px`,
          scale: String(
            interpolate(enter, [0, 1], [0.85, 1]) -
              0.42 * exit,
          ),
          opacity: 1 - 0.25 * exit,
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
                from={lead + Math.round(s.at * fps)}
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
              from={lead + contentFrames}
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
            <Sequence durationInFrames={lead} name="First frame, held">
              <Freeze frame={0}>
                <Video
                  src={staticFile(`${name}.mp4`)}
                  muted
                  style={{ width: WIN_W, height: WIN_H }}
                />
              </Freeze>
            </Sequence>
            {timeline.highlights.map((h, i) => (
              <Sequence
                key={i}
                from={lead + Math.round((h.at + 0.35) * fps)}
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

// Where the camera is at t seconds into the recording: a spring from the
// last focus to the current one, each scaled to fill the window with its
// rectangle, at most 1.9 times, and kept inside the recording.
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
          config: { damping: 200 },
          durationInFrames: Math.round(0.9 * fps),
        });
  const s = prev.s + (cur.s - prev.s) * p;
  const cx = prev.cx + (cur.cx - prev.cx) * p;
  const cy = prev.cy + (cur.cy - prev.cy) * p;
  const clamp = (v: number, lo: number, hi: number) =>
    Math.min(hi, Math.max(lo, v));
  return {
    s,
    x: clamp(WIN_W / 2 - cx * WIN_W * s, WIN_W - WIN_W * s, 0),
    y: clamp(WIN_H / 2 - cy * WIN_H * s, WIN_H - WIN_H * s, 0),
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
    [0, 0.25 * fps, durationInFrames - 0.4 * fps, durationInFrames],
    [0, 1, 1, 0],
    { extrapolateLeft: "clamp", extrapolateRight: "clamp" },
  );
  const grow = spring({ frame, fps, config: { damping: 12 } });
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
        boxShadow: `0 0 36px ${ORANGE}99, inset 0 0 36px ${ORANGE}33`,
        opacity,
        scale: String(1.04 - 0.04 * grow),
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
  const pop = spring({ frame: since, fps, config: { damping: 15, mass: 0.5 } });
  return (
    <AbsoluteFill
      style={{ justifyContent: "flex-end", alignItems: "center" }}
    >
      <div
        style={{
          marginBottom: 34,
          padding: "16px 36px",
          borderRadius: 24,
          backgroundColor: "rgba(20,21,28,0.82)",
          border: "1px solid rgba(255,255,255,0.1)",
          boxShadow: "0 20px 60px rgba(0,0,0,0.5)",
          fontSize: 48,
          fontWeight: 700,
          color: "white",
          letterSpacing: -0.5,
          whiteSpace: "nowrap",
          maxWidth: 1700,
          textAlign: "center",
          opacity: pop,
          scale: String(0.9 + 0.1 * pop),
          translate: `0px ${24 * (1 - pop)}px`,
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
  const out = interpolate(since, [1.6 * fps, 1.9 * fps], [1, 0], {
    extrapolateLeft: "clamp",
    extrapolateRight: "clamp",
  });
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
          frame: since - i * 4,
          fps,
          config: { damping: 9, mass: 0.5 },
        });
        return (
          <div
            key={i}
            style={{
              minWidth: 70,
              padding: "12px 22px",
              borderRadius: 14,
              background: "linear-gradient(180deg, #343747, #262833)",
              border: "1px solid #50546a",
              borderBottom: "6px solid #14151b",
              boxShadow: `0 16px 40px rgba(0,0,0,0.55), 0 0 30px ${PINK}44`,
              fontFamily: MONO,
              fontSize: 40,
              fontWeight: 700,
              color: "white",
              textAlign: "center",
              opacity: Math.min(1, pop * 1.5),
              scale: String(0.5 + 0.5 * pop),
              translate: `0px ${30 * (1 - pop)}px`,
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
  const pop = (delay: number) =>
    spring({
      frame: frame - delay - 0.3 * fps,
      fps,
      config: { damping: 14, mass: 0.6 },
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
          fontSize: 120,
          fontWeight: 700,
          letterSpacing: -4,
          backgroundImage: `linear-gradient(90deg, ${PINK}, ${ORANGE})`,
          backgroundClip: "text",
          WebkitBackgroundClip: "text",
          color: "transparent",
          opacity: pop(0),
          scale: String(0.85 + 0.15 * pop(0)),
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
          backgroundColor: "rgba(20,21,28,0.85)",
          border: "1px solid rgba(255,255,255,0.12)",
          opacity: pop(6),
          translate: `0px ${30 * (1 - pop(6))}px`,
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
          opacity: pop(12),
        }}
      >
        github.com/ian-pascoe/neopane
        {alsoIn ? (
          <span style={{ color: "#8a8d99" }}> · {alsoIn}</span>
        ) : null}
      </div>
    </AbsoluteFill>
  );
};

