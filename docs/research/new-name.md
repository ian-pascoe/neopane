# Name availability: neopane, panvim, nvimd, neovimd, npane, nvpane

Checked 2026-11-05 (all checks that say "404" were live HTTP requests that day).
"Free" means no hit at that source; names can be taken later.

## Result per name

| Name | crates.io | Homebrew | Distros | npm / PyPI | GitHub | Noise / verdict |
|---|---|---|---|---|---|---|
| **neopane** | free | free | free | free | no repo `neopane` or `neopane.nvim`; **user `neopane` exists** (since 2018, 3 forks/test repos, 0 followers, inactive) | Search noise from "NeoPanel" repos (hexesdesu/NeoPanel 12★, etc., unrelated web panels). Cleanest of the four. |
| **panvim** | free | free | free | free | no repo `panvim`; user `panvim` free | Search noise: kdheepak/panvimdoc (326★, on awesome-neovim) is a different name (`panvimdoc`), not a collision. Reads as "pan" + "vim"; also near-miss with Pandoc-vim tools. |
| **nvimd** | free | free | free | free | **ahdinosaur/nvimd** (9★, last push 2023-10-25, not archived): "long-running neovim server" CLI, same niche, plus 5 other zero-star repos named `nvimd`. **Org `nvimd` exists** (empty, created 2025-02-06). | Direct conceptual collision with ahdinosaur/nvimd. Also search noise with `nvimdots`, `nvimdev`. Not on awesome-neovim by that name. |
| **neovimd** | free | free | free | free | no repo; user free | Free everywhere. Contains the full word "Neovim" (see Q2: fine if not presented as official). Long to type; search noise with "neovimdots". |
| **npane** (shorthand) | free | free | free | free | no repo `npane`; **user `npane` exists** (empty, created 2025-06-28). | Search noise: Npanel / NPanel (Leiren/Npanel 85★, npanel-dev/*). AUR substring search returns byteonpanel-git, plainpanel, which are not `npane`. |
| **nvpane** (shorthand) | free | free | free | free | no repo; user free (`nvpanel` repos are unrelated) | Free everywhere checked. |

No name is a known product or trademark in what I found (no trademark database was searched; see Unchecked).

## Evidence by source

### a. crates.io
`GET https://crates.io/api/v1/crates/<name>` returned 404 for all six.

### b. Homebrew
`https://formulae.brew.sh/api/formula/<name>.json` and `/api/cask/<name>.json`: 404 for all six formulae and all six casks.

### c. Distro packages
- Repology (aggregates Debian, Ubuntu, Arch, AUR, nixpkgs, MacPorts, Homebrew and others): `https://repology.org/api/v1/project/<name>` returned `[]` for all six. The same endpoint returns data for `neovim`, so the API works. This is the broadest single check.
- Debian and Ubuntu, package name search (`https://packages.debian.org/search?keywords=<name>&searchon=names&suite=all&section=all`, same on packages.ubuntu.com): "Sorry, your search gave no results" for all six.
- Debian file-contents search for `bin/<name>` (`https://packages.debian.org/search?searchon=contents&keywords=bin/<name>&mode=exactfilename&suite=unstable&arch=any`): no results for all six. Ubuntu contents search was not run (unchecked).
- Arch official: `https://archlinux.org/packages/search/json/?q=<name>` returned no results for all six.
- AUR: `https://aur.archlinux.org/rpc/v5/search/<name>?by=name` (and `by=name-desc`) returned 0 for neopane, panvim, nvimd, neovimd. npane returned byteonpanel-git and plainpanel, and nvpane returned r-cnvpanelizer: substring matches, not the names.
- nixpkgs: `pkgs/by-name/<2 letters>/<name>/package.nix` on master returned 404 for all six, and Repology shows nothing. This only checks the by-name layout; search.nixos.org and GitHub code search (needs auth) were **not** checked, so a nixpkgs attribute under another path is unchecked.
- MacPorts: `https://ports.macports.org/api/v1/port/<name>/` returned 404 for all six.

### d. npm / PyPI
`https://registry.npmjs.org/<name>` and `https://pypi.org/pypi/<name>/json`: 404 for all six.

### e. GitHub
GitHub search API (`/search/repositories?q=<name>+in:name`, filtered to exact name, also `<name>.nvim`) and `/users/<name>`:
- `nvimd`: ahdinosaur/nvimd (9★, 2023-10-25), micheewang/nvimd, Selora/NVIMd, tu-nguyen-iris/nvimd, stephenfairchild/nvimd, ussarnevo/nvimd (all 0★). Org `nvimd`: exists, 0 repos.
- `neopane`: user exists (neopane, 2018, repos addressbook-1/history/test). No repo.
- `npane`: user exists (empty, 2025-06-28).
- `panvim`, `neovimd`, `nvpane`: no exact repo, no user/org.
- No `<name>.nvim` repos for any name.

Taking a GitHub repo name is not blocked by an existing user, but `github.com/<name>` as an org is: neopane, nvimd and npane logins are taken.

### f. awesome-neovim and dotfyle
- awesome-neovim README (`https://raw.githubusercontent.com/rockerBOO/awesome-neovim/main/README.md`, fetched 2026-11-05): zero matches for neopane, neovimd, npane, nvpane. `panvim` matches only kdheepak/panvimdoc. `nvimd` matches only `nvimdev/*` and ayamir/nvimdots, not a plugin named nvimd.
- dotfyle.com: **unchecked**. I did not find a working search endpoint (search URL guess 404; plugin page 500).

### g. Other
- Search noise summarised in the table: NeoPanel (neopane), panvimdoc (panvim), nvimdots/nvimdev (nvimd), Npanel (npane).
- No trademark database (USPTO/EUIPO) searched: unchecked. No web search for well-known products was done beyond the above.

## Neovim name policy

**No dedicated trademark or branding policy was found.**

- neovim.io: `/logos-and-brand/`, `/brand`, `/trademark` return 404. `/` and `/charter/` exist; the charter and homepage had no trademark or naming text in my grep (not read in full).
- neovim/neovim repo root and `.github/` (listed via the API on 2026-11-05) have no BRANDING or TRADEMARK file. README, CONTRIBUTING.md, MAINTAIN.md, AGENTS.md, BUILD.md and INSTALL.md have no trademark/brand text, apart from the README logo image.
- `LICENSE.txt` (Apache-2.0 part, section 6): "This License does not grant permission to use the trade names, trademarks, service marks, or product names of the Licensor, except as required for reasonable and customary use in describing the origin of the Work". https://github.com/neovim/neovim/blob/master/LICENSE.txt
- Maintainer statements in discussions:
  - https://github.com/neovim/neovim/discussions/24206 (2023-06-29): justinmk: "As long as no third-party product is presented as being part of the Neovim project, it's fine to mention Neovim by name."
  - https://github.com/neovim/neovim/discussions/32267 (2025-01-31, Neovide logo): justinmk: "As long as users don't confuse a project as being part of the Neovim brand, there is no problem." clason: the main distinction should be the name ("D" for Neovide). The asker notes no trademark was found.

Practical reading (my inference, not a Neovim statement): names embedding "nvim"/"neovim" are common (Neovide, nvim-*, neomux) and tolerated as long as the project does not look official. `neovimd` is the one most likely to read as official; `nvimd` less so; neopane/panvim/npane avoid the question.

## What existing Neovim multiplexer/daemon projects call themselves

Stars/push dates from the GitHub API, 2026-11-05.

| Name | Repo | What | State |
|---|---|---|---|
| nmux | tweekmonster/nmux | multiplexer for Neovim processes | 112★, last push 2017 |
| nvimux | hkupty/nvimux | Neovim as a tmux replacement | 450★, last push 2022 |
| neomux | nikvdp/neomux | control Neovim from shells inside Neovim | 394★, 2026-06 |
| neomux (others) | 4sp1/neomux | manage headless Neovim, spawn Neovide (`nx` alias) | 0★, 2025-12 |
| neoplex | j-ace-svg/neoplex | session-based Neovim multiplexer ("neomux was taken") | 0★, 2023 |
| nvmux | Shooooooooo/nvmux | no description | 0★, 2026-11 |
| nvimd | ahdinosaur/nvimd | long-running Neovim server for a terminal workflow | 9★, 2023 |
| neorex | alexrlocke/neorex | Rex-native frontend for Neovim (the upstream) | 284★, 2026-11 |

Takeaway: `nmux`, `nvimux`, `neomux`, `neoplex`, `nvimd` are all used by prior projects in this niche; `neopane`, `panvim`, `npane`, `nvpane`, `neovimd` are not.

## Unchecked

- dotfyle.com plugin listing.
- search.nixos.org and GitHub code search for nixpkgs; Ubuntu file-contents search.
- Trademark registries; general web search for products named like these.
- Bare crates.io API was queried with a custom User-Agent; availability could differ for reserved or yanked names (a 404 means no crate by that name).

---

# Follow-up: oriel (primary), dormer, transom (backups)

Checked 2026-11-05, same methods and endpoints as above (crates.io API with owner endpoints; Homebrew formula/cask JSON; Debian/Ubuntu name and Debian contents search; Arch and AUR RPC; nixpkgs `pkgs/by-name`; MacPorts API; Repology; npm; PyPI; GitHub search/users API; awesome-neovim README).

| Check | **oriel** | dormer | transom |
|---|---|---|---|
| crates.io | **TAKEN** (placeholder, see below) | free (404) | free (404) |
| Homebrew formula / cask | free / free | free / free | free / free |
| Debian / Ubuntu (names; Debian `bin/` contents) | free | free | free |
| Arch official / AUR | free / free | free / free | free / AUR substring only (`r-transomics2cytoscape`, not the name) |
| nixpkgs by-name / MacPorts / Repology | free / free / no project | free / free / no project | free / free / no project |
| npm | name was published then **fully unpublished** 2026-06-21 (registry returns no versions); currently unusable-looking but not clearly held | free (404) | **TAKEN**: `transom` 0.1.0 (2026-10), "A calm Postgres viewer in your browser", ships a CLI (`bin: transom`) |
| PyPI | **TAKEN**: `oriel` 0.1.0 (jw-y, "Turn any folder of Markdown into a beautiful website", last upload 2026-07-21) | **TAKEN**: `dormer` 0.2, "save/restore i3 workspace->output mappings" (palfrey/dormer) | **TAKEN**: `transom` 0.0.1 (empty summary) |
| GitHub user/org login | taken: `oriel` (person "Oriel Frigo", since 2008, 6 repos) | taken: `dormer` (user since 2011, 1 repo) | taken: `Transom` (since 2013, 5 repos) |
| GitHub repos named exactly | ~90 repos, mostly 0-3★ and mostly 2026 (see below) | ~8, all 0★ | ~30, 0-5★ |
| awesome-neovim | no match | no match | no match |
| dotfyle | unchecked | unchecked | unchecked |

## oriel

- **crates.io: taken.** `oriel` 0.0.0, a single version published 2026-10-23, 10 downloads, owner user `nexxeln`, description "A desktop UI framework for Rust, built for deterministic testing and agent-driven verification. Early development.", no repository link. It looks like a name reservation for an early project; no activity since. Owner-transfer requests are possible on crates.io but are not guaranteed. Source: https://crates.io/api/v1/crates/oriel and `/owner_user`. No public GitHub repo for it under `nexxeln` was found.
- **PyPI taken, npm formerly published/now unpublished**, see table. A neorex binary would not need these, but `pip`/`npm` users could be confused.
- **GitHub:** many unrelated, mostly brand-new, repos named `oriel`. The most starred: wojciech-graj/oriel 57★ (interpreter for the 1991 Oriel scripting language, last push 2023-11-19). Several are desktop/terminal-adjacent and current, so the name is crowded:
  - domus-apps/oriel, 4★, 2026-10: keyboard-driven window manager for macOS
  - ParadoxInfinite/oriel, 9★, 2026-09: local GUI for Colima/Docker and MCP server
  - Irfanwani/oriel, 2★, 2026-11: macOS auto-tiling desktop
  - eymardfreire/oriel, 0★: "A terminal desk for markets, news, weather, and sports"
  - FruitieX/oriel, 0★: low-latency remote desktop in Rust
  - highercomve/Oriel, 1★: desktop and mobile apps in Zig
  Source: `https://api.github.com/search/repositories?q=oriel+in:name`.
- **Search for "oriel neovim" / "oriel terminal":** no Neovim or terminal-multiplexer project called oriel found. A repo search of `oriel` with `terminal`/`neovim`/`nvim` returned only the repos above and "O'Reilly" misspellings ("Orielly"). General web search for "oriel neovim terminal" returned only generic Neovim terminal-multiplexer articles, none naming oriel.
- **Notable products/trademarks:**
  - **Oriel**, a 1991 graphical scripting language for Windows 3.x from the LeBlond Group's Power Tools books, with a Wikipedia page: https://en.wikipedia.org/wiki/Oriel_(scripting_language). Historical; a Rust interpreter exists (wojciech-graj/oriel).
  - ORIEL registered trademarks exist in unrelated fields: Newport Corporation (US serial 75927079, registered 2002; optics, lasers by field of use) https://www.trademarkia.com/oriel-75927079 ; Oriel Ventures (Cyprus) Limited (US reg. 5189059, 2017) https://trademarks.justia.com/867/65/oriel-86765588.html ; a VIBE INC. EU filing 019259661. I did not read the goods/services classes in detail and did not search the Class 9/42 software register: **trademark clearance for software is unchecked**.
  - "oriel" is also an architectural term (a bay window), which fits the pane/window theme and also means plenty of non-software search noise.
- **Neovim policy:** the name contains no "nvim"/"neovim", so the Q2 findings do not apply.

## dormer

- crates.io, npm, Homebrew, distros: free.
- PyPI `dormer` 0.2 is palfrey's i3 workspace-to-output saver (GitHub palfrey/dormer, 0★, 2025-08), a window-manager utility, adjacent in spirit but tiny.
- GitHub: ~8 repos named `dormer`, all 0★ (e.g. bytes2bit/dormer, a Linux/Wayland living wallpaper, 2026-10). User `dormer` taken (since 2011). Also small "Dormer" (olajowon/dormer-*) repos.
- No awesome-neovim match; no notable product or trademark found (not searched in registries).

## transom

- crates.io, Homebrew, distros: free.
- npm `transom` is taken by a Postgres viewer with a CLI of the same name (0.1.0, 2026-10). Also npm `transomjs/transom-core` (10★, low-code REST API framework, 2025-11) and PyPI `transom` 0.0.1.
- GitHub: ~30 repos named `transom`, mostly 0-5★ and current (2026), several macOS/desktop window tools (aydinmrnv/transom, remote app windows macOS to Windows, 5★; domus-apps/transom, macOS brightness; darvid/transom, macOS browser-profile manager; mangerle/transom, Rust desktop app, 0★). Org `Transom` taken (since 2013, 5 repos). Search noise from TransOMCS and "transomics".
- Product note: the Vornado Transom is a fan (Home Assistant integration repo exists). No trademark search done.

## Summary

Cleanest of the three for registries is **dormer** (crates.io, npm, all distros free), then **oriel** (but crates.io `oriel` is reserved by nexxeln, and PyPI is taken). **transom** loses on npm (CLI of the same name). On GitHub, `oriel` is the most crowded name by far (~90 repos, several in the macOS window/terminal space). Unchecked for all three: dotfyle, trademark registers beyond the hits above, search.nixos.org, Ubuntu contents search.
