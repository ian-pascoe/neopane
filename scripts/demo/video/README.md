# The demos' social cut

A Remotion project that turns a demo tape's VHS recording into a video for
Reddit and Discord: the recording in a window on an animated background,
zooms and outlines where the tape's `# focus:` and `# highlight:` marks say,
big captions and key caps, a title and an end card, and music.

Render one with `scripts/demo/social.sh NAME` (or `record.sh NAME`, which
records it first). `npm run dev` opens Remotion Studio on the last one
rendered. The marks are data from `public/NAME.json`, written by
`scripts/demo/tape.py`, so Studio can't edit them; change the tape instead.
Each tape gets a `<Composition>` in `src/Root.tsx`, with its title text.

Remotion is free for individuals and companies of up to 3 people.
