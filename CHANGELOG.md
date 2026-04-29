# Changelog

All notable changes to this project will be documented in this file.

## [0.0.3] - 2026-04-29

### Removed
- Remove `list` command and the course listing feature.

## [0.0.2] - 2026-04-29

### Added
- GitHub Actions workflow for Rust.

### Changed
- Reduce CLI surface and clean warnings.
- Simplify download CLI to `course-id` + lecture selectors; merge lecture selectors into `--lecture`.

### Fixed
- Download behavior fixes: keep lecture range numbering consistent; restore `export_courses` method signature.
- Login flow fixes: support file-based login; print captcha image without overwriting terminal history; stop after captcha login; ignore `login.toml`.
- Help output: hide login args from download help.

## [0.0.1] - 2026-04-29

- Initial release.
