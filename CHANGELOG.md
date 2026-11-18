# Changelog

## [0.2.2](https://github.com/spiritledsoftware/neopane/compare/v0.2.1...v0.2.2) (2026-11-18)


### Documentation

* drop research notes no planned work needs ([#246](https://github.com/spiritledsoftware/neopane/issues/246)) ([1c1b5fb](https://github.com/spiritledsoftware/neopane/commit/1c1b5fb5203f2d1abc8ecc14c4a8c63d5e028cd9))

## [0.2.1](https://github.com/spiritledsoftware/neopane/compare/v0.2.0...v0.2.1) (2026-11-18)


### Bug Fixes

* **herdr-app:** find waits for the doc to show the attach's pane ([#233](https://github.com/spiritledsoftware/neopane/issues/233)) ([8633456](https://github.com/spiritledsoftware/neopane/commit/8633456f27b9bb695b75028a0849bb996e7c82c0))
* say why an attach has no tty ([#238](https://github.com/spiritledsoftware/neopane/issues/238)) ([5417931](https://github.com/spiritledsoftware/neopane/commit/5417931735603edbabc4449b1c5f253a0a08fed9))
* **tmux:** events waits until tmux lists its waiters before it reads ([#242](https://github.com/spiritledsoftware/neopane/issues/242)) ([b5fb2ed](https://github.com/spiritledsoftware/neopane/commit/b5fb2ed1e628079ceaf667841625ec8aac903116))
* **tmux:** read the layout again after a pane resizes with no layout change ([#237](https://github.com/spiritledsoftware/neopane/issues/237)) ([7ed9345](https://github.com/spiritledsoftware/neopane/commit/7ed9345d518a76af59caeef0a99bd75e0c2fec42))

## [0.2.0](https://github.com/spiritledsoftware/neopane/compare/v0.1.0...v0.2.0) (2026-11-18)


### Features

* relay program status (OSC 7501) from terminal buffers to the Neovim panes showing them (ADR 0021) ([#224](https://github.com/spiritledsoftware/neopane/issues/224)) ([c95ece5](https://github.com/spiritledsoftware/neopane/commit/c95ece57438d78ecd8ec5889ed8c75806dc47405))


### Bug Fixes

* find a command ended when macOS reports its exit before it can be reaped ([#228](https://github.com/spiritledsoftware/neopane/issues/228)) ([a9dbad2](https://github.com/spiritledsoftware/neopane/commit/a9dbad2cec465594d6c5995bc628a18e15ee56ae))
* **herdr:** give a lone pane's pty back the column herdr keeps after a focus ([#229](https://github.com/spiritledsoftware/neopane/issues/229)) ([d2a5b2e](https://github.com/spiritledsoftware/neopane/commit/d2a5b2ebadf7bc6b657ca8104e2a03717935d377))
* three bugs (command ends, attach exits, herdr tab pages a column short), three flaky tests, CI runs every target ([#223](https://github.com/spiritledsoftware/neopane/issues/223)) ([99aa16b](https://github.com/spiritledsoftware/neopane/commit/99aa16b7acf0e3aa56caec3b425dfc9fde432c61))
