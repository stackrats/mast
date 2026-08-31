# ADR-0010: mast:// — links that may knock but never enter

- **Status:** accepted
- **Date:** 2026-08-31
- **Applies to:** `mast-desktop` (deep-link + single-instance plugins, `DeepLinkEvent`, `take_deep_links`), `clients/desktop-vue/src/lib/deeplink.ts`

## Question

Getting a project into Mast finally takes one dialog (create or clone), but
reaching that dialog still takes prose: a README that says "install Mast,
open it, click Add, paste this URL". Every other step of team onboarding
became a link long ago. What should `mast://` links be able to do — and,
more importantly, what must they never do?

## Decision

**Two links, and a rule that outranks both.** `mast://clone?url=…` opens
the add-project dialog in From Git mode with the URL prefilled;
`mast://project/<name-or-path-suffix>` selects a project. The rule: a link
may only **navigate or prefill, never act**. Any webpage can fire a
registered URL scheme, so a link that cloned, started, or stopped anything
by itself would be drive-by control of the developer's machine. The person
reviews the prefilled dialog and clicks the button; unknown actions parse
to null and are ignored with a note in the activity feed, never guessed at.

**One window now, not two.** The single-instance plugin is how a link
reaches an app that is already running: the second launch forwards its argv
and exits, and the existing window comes to the front. This deliberately
retires the desktop's second-window-read-only behavior (plan §1's flock) —
with the daemon serving the CLI full rights, a second GUI window had no
job, and tray users double-clicking the icon now get the window they meant
instead of a read-only twin. The flock stays: it still guards genuinely
separate builds and the CLI's embedded-engine fallback.

**Delivery is two-phase** because links race the webview: URLs present at
launch are parked in managed state and drained by `take_deep_links` after
the frontend subscribes to `DeepLinkEvent`; anything later arrives as the
event. The frontend subscribes first, then drains — that order is what
closes the gap.

## Consequences

- Scheme registration rides the existing channels: bundler config
  (Info.plist / registry / `.desktop` MimeType) for installed builds, plus
  best-effort runtime registration on Linux and Windows for dev builds.
  macOS and Windows behavior is compile-checked by CI but — like every
  platform integration before it (ADR-0006) — considered unverified until a
  field round clicks a real link there.
- A clone link's URL still passes through the engine's refusals
  (credentials in http(s) URLs, occupied targets); the dialog is a
  convenience, not a bypass.
- Linux hand-test: run the app, then `xdg-open "mast://clone?url=…"` — the
  window should rise with the dialog filled.
