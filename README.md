# eARC Volume Overlay for LG webOS

Shows your soundbar's or receiver's volume as a number on rooted LG webOS TVs.
When the TV controls an HDMI ARC/eARC audio device, LG's own volume display
often shows no level; this puts a small indicator in the bottom-right corner
instead.

![Volume overlay showing 24 in the bottom-right corner](docs/overlay-preview.jpg)

This is a fork of [Nikolay1243/Webos-EARC-Volume-Overlay](https://github.com/Nikolay1243/Webos-EARC-Volume-Overlay)
with a native watcher and several new features.

## Features

- **The soundbar's own number.** Reads the volume the soundbar reports over
  HDMI-CEC, never the TV's internal volume.
- **Instant.** Learns your soundbar's volume steps and shows the new level the
  moment you press a key, instead of waiting about half a second for the
  soundbar's reply. Wrong guesses are corrected and relearned.
- **Both remotes.** Changes made with the soundbar's own remote are shown too.
- **Light.** A small native background process (about 260 kB of memory)
  instead of a Node.js one.
- **Compatibility check.** Tells you in 10 seconds whether your soundbar works
  with it.
- **Adjustable size.** Small, Medium or Large.
- Starts automatically after boot, and leaves the remote's controls to the app
  you are using.

## Compatibility

It needs a rooted LG webOS TV with [Homebrew Channel](https://github.com/webosbrew/webos-homebrew-channel),
and a soundbar or receiver that reports its volume over HDMI-CEC. Not every
soundbar does: some accept the TV's volume commands but never send back their
level. Use **Check compatibility** after installing to find out.

| TV | Soundbar | Result |
| --- | --- | --- |
| LG C3 | Polk Signa S4 | Works |
| LG C2 | Samsung HW-Q990B | Not compatible (never reports its level) |

Other combinations are untested.

## Install

**From Homebrew Channel (recommended, gets updates):**

1. In Homebrew Channel, open **Settings → Repositories** and add:
   ```text
   https://raw.githubusercontent.com/danzig666/Webos-EARC-Volume-Overlay/main/repo.json
   ```
2. Install **eARC Volume Overlay** from the app list.
3. Open it from the TV's launcher and select **Enable**.

If you previously added the original project's repository, remove it so the
two don't offer different versions of the same app.

**Manually:** download the `.ipk` from the [latest release](https://github.com/danzig666/Webos-EARC-Volume-Overlay/releases/latest),
install it with webOS Dev Manager or `ares-install`, then open the app and
select **Enable**.

## Setup screen

- **Enable / Disable:** starts or stops the overlay, including after reboots.
- **Test overlay:** shows a sample.
- **Check compatibility:** press volume up and down during the countdown; it
  tells you whether your soundbar reports its level.
- **Overlay size:** Small, Medium or Large.
- **Reset learning:** forgets the learned volume steps, for example after a
  soundbar firmware update.

To uninstall, select **Disable** first, then remove the app.

## How it works

A small Rust program follows the TV's HDMI-CEC log (`/var/log/cecmessages`). It
sees the TV's volume key presses and the soundbar's volume reports, and only
reads: nothing is sent to the soundbar. The overlay itself is a web app that
stays loaded but hidden between uses, so it appears without a start-up delay.

- Learned volume steps are stored per soundbar in `/var/lib/earc-volume-steps`.
- On TVs without a usable CEC log, it falls back to the volume webOS stored
  from the soundbar's last report.
- If nothing confirms your key presses (a soundbar that never reports), the
  overlay stays hidden rather than showing an outdated number.

Log: `tail -f /tmp/earc-volume-overlay.log` on the TV. Start the watcher with
`EARC_VOLUME_DEBUG=1` for detailed timing.

## Build

Needs Node.js 18+, Rust (rustup) and `cargo-zigbuild`
(`pip install ziglang cargo-zigbuild`).

```sh
npm install
npm test
npm run package    # writes the IPK to build/
```

Releases are automatic: when a commit on `main` carries a new version in
`app/appinfo.json`, the Release workflow builds, tests and publishes it with
its Homebrew manifest. Release notes go in `docs/releases/vX.Y.Z.md`.

## License

[MIT](LICENSE). Original project by Nikolay1243.
