# Desktop Development and App Identity

This project has two distinct desktop app identities on macOS:

- Production app:
  - Name: `Minutes.app`
  - Bundle id: `com.useminutes.desktop`
  - Canonical install path: `/Applications/Minutes.app`
- Development app:
  - Name: `Minutes Dev.app`
  - Bundle id: `com.useminutes.desktop.dev`
  - Canonical install path: `~/Applications/Minutes Dev.app`

The split is intentional. macOS TCC permissions such as Microphone, Screen
Recording, Accessibility, Apple Events, and Input Monitoring attach to the
app identity macOS sees, not just to "the code in this repo."

## Why this matters

Testing TCC-sensitive features from multiple app paths or signatures leads to
confusing macOS state:

- permissions appear enabled in System Settings, but the active build still
  gets prompted
- Input Monitoring looks granted for one bundle, but `CGEventTap` still fails
- Screen Recording prompts recur because the process identity changed after a
  rebuild or re-sign

The main causes are:

- launching the raw binary in `target/`
- launching ad-hoc signed bundles
- launching the repo symlink `./Minutes.app`
- mixing `/Applications/Minutes.app` with fresh local build outputs

## Canonical dev workflow

For any desktop work that touches TCC-sensitive features, use exactly one app:

```bash
./scripts/install-dev-app.sh
```

That script:

- builds the desktop bundle with the dev overlay config
- signs it with the configured Developer ID identity
- installs it to `~/Applications/Minutes Dev.app`
- runs the native hotkey diagnostic from the installed app identity
- launches `Minutes Dev.app`

When testing desktop permissions, do not launch:

- `./Minutes.app`
- `target/release/minutes-app`
- `target/release/bundle/macos/Minutes.app`
- older copies of `Minutes Dev.app` from other locations

## Native hotkey diagnostic

The desktop binary has a built-in diagnostic mode that checks whether the
current app identity can start the macOS `CGEventTap` monitor used by the
dictation hotkey:

```bash
~/Applications/Minutes\ Dev.app/Contents/MacOS/minutes-app --diagnose-hotkey
```

Optional keycode override:

```bash
~/Applications/Minutes\ Dev.app/Contents/MacOS/minutes-app \
  --diagnose-hotkey \
  --diagnose-hotkey-keycode 63
```

Interpretation:

- exit `0`: the native hotkey monitor started successfully
- exit `2`: macOS identity / Input Monitoring still blocked the hotkey

This diagnostic is the fastest way to answer "can this exact app identity
create the native hotkey?" without going through the UI.

## Permission map

- Microphone:
  - needed for recording and dictation audio capture
- Screen Recording:
  - needed for screen-context screenshots and some visual desktop testing
  - not required for the dictation hotkey itself
- Input Monitoring:
  - needed for the dictation hotkey `CGEventTap` path
- Accessibility:
  - useful for GUI automation, but not the actual hotkey permission

## Repeated permission prompts

If macOS keeps prompting even though the toggle already looks enabled:

1. Quit all `Minutes` and `Minutes Dev` copies.
2. Reinstall the dev app with `./scripts/install-dev-app.sh`.
3. Launch only `~/Applications/Minutes Dev.app`.
4. Re-run `--diagnose-hotkey` from that installed app.
5. Re-check the specific permission pane for `Minutes Dev`.

If you still see repeated prompts, assume macOS is treating the current build
as a different identity until proven otherwise.

## Guidance for AI agents

When working in this repo:

- treat `~/Applications/Minutes Dev.app` as the canonical desktop dev target
- do not claim a TCC-sensitive feature is fixed based on a raw `target/`
  binary or repo-local bundle
- prefer the built-in `--diagnose-hotkey` probe before speculating about
  Input Monitoring state
- distinguish Screen Recording issues from Input Monitoring issues explicitly
