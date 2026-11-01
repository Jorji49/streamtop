# Install

Prebuilt binaries for Linux (x86_64 and arm64, glibc and musl), macOS (Apple Silicon and Intel), and Windows x86_64 are on [GitHub Releases](https://github.com/Jorji49/streamtop/releases/latest). Each release includes `SHA256SUMS`.

## cargo

```bash
cargo install streamtop
# or, to download a prebuilt binary:
cargo install cargo-binstall && cargo binstall streamtop
```

The minimum supported Rust version is listed in `Cargo.toml` (`rust-version`).

## Homebrew (macOS and Linux)

```bash
brew tap Jorji49/tap
brew install streamtop
```

Recent Homebrew versions may ask you to trust third-party taps first: `brew trust jorji49/tap`.

## Scoop (Windows)

```bash
scoop bucket add streamtop https://github.com/Jorji49/streamtop
scoop install streamtop
```

## winget (Windows)

Manifests are in `dist/winget/` in the repository. The package is not in the winget community index yet, so `winget install` does not work yet.

## Arch Linux

`streamtop-bin` is not on the AUR yet. `dist/aur/PKGBUILD` builds the same package locally from the Linux release tarball:

```bash
git clone https://github.com/Jorji49/streamtop.git
cd streamtop/dist/aur
makepkg -si
```

## Docker

Images for linux/amd64 and linux/arm64 are on GitHub Container Registry:

```bash
docker run -it --rm ghcr.io/jorji49/streamtop:latest <URL>
```

Release tags are also published, for example `ghcr.io/jorji49/streamtop:v1.7.0`.

Prometheus metrics on a non-loopback bind require a token:

```bash
docker run --rm -p 9184:9184 \
  -e STREAMTOP_METRICS_TOKEN=change-me \
  ghcr.io/jorji49/streamtop:latest \
  <URL> --prometheus --metrics-bind 0.0.0.0 \
  --metrics-token "$STREAMTOP_METRICS_TOKEN"
```

Quick Play (`p`) is not available in the container because there is no `mpv` or `ffplay` inside it.

## From source

```bash
git clone https://github.com/Jorji49/streamtop.git
cd streamtop
cargo install --path .
```

## Shell completions and man page

`streamtop completions <SHELL>` prints a completion script for bash, zsh, fish, elvish, or powershell. It covers the main flags and the `check`, `scte35`, `completions`, and `man` subcommands.

```bash
# bash
streamtop completions bash > ~/.local/share/bash-completion/completions/streamtop
# zsh (a directory in your fpath)
streamtop completions zsh > ~/.zfunc/_streamtop
# fish
streamtop completions fish > ~/.config/fish/completions/streamtop.fish
# PowerShell
streamtop completions powershell >> $PROFILE
```

`streamtop man` prints the streamtop(1) man page as roff. `streamtop man check` prints the page for one subcommand.

```bash
mkdir -p ~/.local/share/man/man1
streamtop man > ~/.local/share/man/man1/streamtop.1
streamtop man check > ~/.local/share/man/man1/streamtop-check.1
man streamtop
```
