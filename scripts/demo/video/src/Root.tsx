import { Composition, Folder } from "remotion";
import { calculateDemoMetadata, Demo } from "./Demo";

export const RemotionRoot: React.FC = () => {
  return (
    <Folder name="Social">
      <Composition
        id="tmux-core"
        component={Demo}
        width={1920}
        height={1080}
        fps={30}
        durationInFrames={1200}
        calculateMetadata={calculateDemoMetadata}
        defaultProps={{
          name: "tmux-core",
          windowTitle: "tmux — neopane",
          headline: "One Neovim.",
          subline: "Every window a real tmux pane.",
          alsoIn: "also zellij, herdr and Rex",
          music: null,
          musicVolume: 0.8,
          musicDrop: 12.05,
          bpm: 160,
          timeline: null,
        }}
      />
    </Folder>
  );
};
