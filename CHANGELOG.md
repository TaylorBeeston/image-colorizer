# Changelog
All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [1.1.5](https://github.com/TaylorBeeston/image-colorizer/compare/v1.1.4...v1.1.5) - 2026-06-03

### Other

- Update documentation for GPU pipeline
- Collapse final colorization into spatial shader
- Write final GPU output directly to bytes
- Overlap batch I/O with GPU work
- Reuse GPU renderer across batches
- Move spatial averaging onto the GPU
- Refresh CLI documentation
- Harden config and CLI errors
- Clean up compute shaders
- Add pass one shader test

### Added
- GPU shader tests for the colorization and spatial averaging pipeline.
- Reusable `image-colorizer-core` library crate.
- `image-colorizer serve` local web UI with live parameter/colorscheme previews, uploads, downloads, and config saving.

### Changed
- Converted the project into a Cargo workspace with separate core library and CLI crates.
- Moved spatial averaging and final colorization fully onto the GPU.
- Reused GPU renderer state, scratch buffers, and output byte buffers across batch processing.
- Overlapped image decoding and output saving with GPU work.
- Kept release-plz package versions grouped for coordinated core/CLI publishing.
- Refreshed README usage and pipeline documentation.

### Fixed
- Fixed pass-one shader bounds handling for rounded-up workgroups.
- Replaced panic-prone CLI/config paths with validated errors.
- Added retries when the AUR publish job downloads the freshly published crates.io archive.

## [1.1.4](https://github.com/TaylorBeeston/image-colorizer/compare/v1.1.3...v1.1.4) - 2024-08-12

### Other
- :arrow_up: Bump release-plz version
- :sparkles: Add more colorschemes

## [1.1.3](https://github.com/TaylorBeeston/image-colorizer/compare/v1.1.2...v1.1.3) - 2024-08-07

### Other
- :green_heart: Fix CI

## [1.1.2](https://github.com/TaylorBeeston/image-colorizer/compare/v1.1.1...v1.1.2) - 2024-08-06

### Other
- Config updates
The reason this release is so weird is that I seem to be having trouble with the github actions and
so am hoping manually deploying like this will fix it

## [1.1.1](https://github.com/TaylorBeeston/image-colorizer/compare/v1.1.0...v1.1.1) - 2024-08-06

### Other
- :bug: Use main when resolving colorschemes
- Merge branch 'download-colorschemes'
- :sparkles: Implement resolving remote colorschemes
- :sparkles: Add nord
- :sparkles: Add colorschemes dir

## [1.0.5](https://github.com/TaylorBeeston/image-colorizer/compare/v1.0.4...v1.0.5) - 2024-08-06

### Other
- :sparkles: Update definition of colorschemes to simpler format

## [1.0.4](https://github.com/TaylorBeeston/image-colorizer/compare/v1.0.3...v1.0.4) - 2024-07-29

### Other
- :memo: Update Readme about AUR support

## [1.0.3](https://github.com/TaylorBeeston/image-colorizer/compare/v1.0.2...v1.0.3) - 2024-07-29

### Other
- AUR Support

## [1.0.2](https://github.com/TaylorBeeston/image-colorizer/compare/v1.0.1...v1.0.2) - 2024-07-28

### Other
- :green_heart: Fix CI command and add emply changelog
