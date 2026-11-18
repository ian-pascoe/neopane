# Changelog

## [0.2.0](https://github.com/spiritledsoftware/neopane/compare/v0.1.0...v0.2.0) (2026-11-18)


### Features

* relay program status (OSC 7501) from terminal buffers to the Neovim panes showing them (ADR 0021) ([#224](https://github.com/spiritledsoftware/neopane/issues/224)) ([c95ece5](https://github.com/spiritledsoftware/neopane/commit/c95ece57438d78ecd8ec5889ed8c75806dc47405))


### Bug Fixes

* find a command ended when macOS reports its exit before it can be reaped ([#228](https://github.com/spiritledsoftware/neopane/issues/228)) ([a9dbad2](https://github.com/spiritledsoftware/neopane/commit/a9dbad2cec465594d6c5995bc628a18e15ee56ae))
* **herdr:** give a lone pane's pty back the column herdr keeps after a focus ([#229](https://github.com/spiritledsoftware/neopane/issues/229)) ([d2a5b2e](https://github.com/spiritledsoftware/neopane/commit/d2a5b2ebadf7bc6b657ca8104e2a03717935d377))
* three bugs (command ends, attach exits, herdr tab pages a column short), three flaky tests, CI runs every target ([#223](https://github.com/spiritledsoftware/neopane/issues/223)) ([99aa16b](https://github.com/spiritledsoftware/neopane/commit/99aa16b7acf0e3aa56caec3b425dfc9fde432c61))
