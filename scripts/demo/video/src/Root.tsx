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
          alsoIn: "also zellij, herdr, the Herdr app and Rex",
          music: null,
          musicVolume: 0.8,
          musicDrop: 12.05,
          bpm: 160,
          timeline: null,
        }}
      />
      <Composition
        id="zellij-core"
        component={Demo}
        width={1920}
        height={1080}
        fps={30}
        durationInFrames={1200}
        calculateMetadata={calculateDemoMetadata}
        defaultProps={{
          name: "zellij-core",
          windowTitle: "zellij — neopane",
          headline: "One Neovim.",
          subline: "Every window a real zellij pane.",
          alsoIn: "also tmux, herdr, the Herdr app and Rex",
          music: null,
          musicVolume: 0.8,
          musicDrop: 12.05,
          bpm: 160,
          timeline: null,
        }}
      />
      <Composition
        id="herdr-core"
        component={Demo}
        width={1920}
        height={1080}
        fps={30}
        durationInFrames={1200}
        calculateMetadata={calculateDemoMetadata}
        defaultProps={{
          name: "herdr-core",
          windowTitle: "herdr — neopane",
          headline: "One Neovim.",
          subline: "Every window a real herdr pane.",
          alsoIn: "also tmux, zellij, the Herdr app and Rex",
          music: null,
          musicVolume: 0.8,
          musicDrop: 12.05,
          bpm: 160,
          timeline: null,
        }}
      />
      <Composition
        id="rex-core"
        component={Demo}
        width={1920}
        height={1080}
        fps={30}
        durationInFrames={1200}
        calculateMetadata={calculateDemoMetadata}
        defaultProps={{
          name: "rex-core",
          windowTitle: "Rex — neopane",
          headline: "One Neovim.",
          subline: "Every window a real Rex block.",
          alsoIn: "also tmux, zellij, herdr and the Herdr app",
          music: null,
          musicVolume: 0.8,
          musicDrop: 12.05,
          bpm: 160,
          timeline: null,
        }}
      />
      <Composition
        id="herdr-app-core"
        component={Demo}
        width={1920}
        height={1080}
        fps={30}
        durationInFrames={1200}
        calculateMetadata={calculateDemoMetadata}
        defaultProps={{
          name: "herdr-app-core",
          windowTitle: "Herdr — neopane",
          headline: "One Neovim.",
          subline: "Every window a real Herdr app pane.",
          alsoIn: "also tmux, zellij, herdr and Rex",
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
