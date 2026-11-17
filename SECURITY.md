# Security policy

## Reporting a vulnerability

Report it privately through GitHub's private vulnerability reporting:
[report a vulnerability](https://github.com/spiritledsoftware/neopane/security/advisories/new) (the repository's **Security**
tab, then **Report a vulnerability**). Please don't open a public issue, pull request or discussion for it.

Include what you can of:

- what the problem is, and what an attacker gains;
- the neopane version (`neopane --version`) or commit, the OS, and the pane provider;
- steps to reproduce, or a proof of concept;
- any logs that help, with private details removed.

## What happens next

We read every report, reply on the advisory, and keep you posted while we confirm and fix it. Once a fix is released,
we publish the advisory and credit you if you wish. This is a small project: there is no bounty and no fixed response
time.

## Supported versions

Only the latest release gets fixes, and `main`. There are no release branches.

## Scope

neopane runs as you, for you. These are in scope, among others:

- another local user reaching your daemon: its socket, its Neovim's socket, or the files in its directory
  (`/tmp/neopane-<uid>/`, made mode 0700);
- a pane provider's events or replies, or an attach, making the daemon do something it shouldn't;
- `install.sh` or the release archives installing something other than what was released.

Your own config (`~/.config/neopane/init.lua`) and Neovim config run with your rights by design: what they do is not a
vulnerability in neopane.
