# Contributing to OpenClips

Thanks for your interest. A few rules keep the project coherent.

## Ground rules

- Everything in the repository is in English: code, comments, commit messages, documentation and UI strings.
- Do not use em dashes anywhere. Use commas, parentheses or separate sentences.
- Comments are for non obvious decisions, invariants and platform quirks. Do not narrate what the code already says.
- Errors are values. Use `Result` with the crate error types. Avoid `unwrap` outside tests and clearly infallible cases.
- Keep `core` free of platform code. Anything that touches the OS, the screen, audio devices or an encoder belongs in `capture` behind the platform trait, or in `app` if it is purely about the desktop shell.

## Workflow

1. Fork and branch from `main`.
2. Make your change, with tests for platform independent logic.
3. Run the same checks as CI before opening a pull request:

   ```text
   cargo fmt --all --check
   cargo clippy --workspace --all-targets -- -D warnings
   cargo test --workspace
   ```

4. Open a pull request with a clear description of the change and, for capture or encoder work, the GPU and driver it was tested on.

## Commit messages

Use [Conventional Commits](https://www.conventionalcommits.org/) in the imperative mood, for example:

```text
feat: add per source audio volume control
fix: keep tray icon alive when the main window is hidden
docs: document the GStreamer plugin requirements
```

Keep the subject line under 72 characters. Explain the why in the body when it is not obvious from the diff.

## Reporting bugs

Please include your Windows version, GPU and driver version, the GStreamer version (`gst-inspect-1.0 --version`), and the relevant portion of the log file from `%LOCALAPPDATA%\OpenClips\data\logs`.

## Releasing

Releases are made from the Actions tab with the Release workflow. Choose patch, minor or major (or type an exact version); the workflow bumps the version in `Cargo.toml`, commits and tags it, builds the installer and the portable zip and publishes the GitHub release with a `SHA256SUMS.txt`. Installed copies of the app pick the release up on their next start. `scriptselease.bat -Patch` (or `-Minor`, `-Major`, `-V x.y.z`) does the same from a local Windows machine with NSIS and `gh` installed.
