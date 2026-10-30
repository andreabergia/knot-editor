# D064 — Theming system

Status: 🚧 in progress. Item of the
[daily-driver plan](daily-driver.md); depends on D070 (settings).

## Goal

Themes become extension contributions, like languages. A bundled
`@knot/themes` package ships popular permissively licensed themes, a Select
Theme command switches between them and remembers the choice, and every
native surface takes its colors from the active theme instead of constants.
Personal config can register its own theme, or derive one from a registered
theme by changing a few keys.

## Design

- **Registration mirrors `languages`.** `themes.register(definition)` returns
  a lifecycle-owned `Disposable`. The latest registration of a name wins, and
  disposal or teardown reveals the previous one. `themes.list()` returns
  each effective theme's name and appearance; `themes.get(name)` returns the
  full registered definition, or null.
- **A definition** is `{ name, appearance: "dark" | "light", colors, syntax }`.
  `colors` maps flat dotted UI keys to CSS hex colors (`#rrggbb` or
  `#rrggbbaa`). `syntax` maps capture names to
  `{ color?, fontStyle?: "italic", fontWeight?: number, underline?: boolean,
  strikethrough?: boolean }`. Theme families are not modelled: a package
  registers several themes.
- **No `extends`.** Themes are code, so derivation is a spread over
  `themes.get`:
  ```ts
  const base = await themes.get("One Dark");
  await themes.register({ ...base, name: "My Dark",
    colors: { ...base.colors, "editor.background": "#101010" } });
  ```
- **Color keys are definitions, like settings.** A definition gives a key a
  description and a default per appearance; a default is a hex color or the
  name of another key, as in VS Code's `contributes.colors`:
  ```ts
  await themes.defineColor({ key: "diff.add", description: "Added lines",
    defaults: { dark: "#2ea04366", light: "#2ea04340" } });
  await themes.defineColor({ key: "git.untracked", description: "Untracked files",
    defaults: { dark: "success", light: "success" } });
  ```
  Native keys are defined in one Rust table beside the code that paints them,
  because native surfaces paint before packages start and when they fail, so
  their defaults must not depend on a package; `editor.tabSize` works the
  same way for settings. Extension definitions are lifecycle-owned, and
  defining a key native code or a live lifecycle already defines rejects, as
  `settings.define` does. The native set starts small, derived from what
  surfaces draw today (editor, tabs and panes, docks, trees, pickers and
  transient fields, completion, prompts, terminal and its 16 ANSI colors,
  and `error`/`warning`/`info`/`success`); later surfaces add their keys when
  they land.
- **Theme color maps are open.** A theme may set any key; a key nobody has
  defined is kept and inert, so a theme can style an extension that loads
  later, and typos are silent as with settings. Syntax keys stay the closed
  capture vocabulary (resolved by longest dotted prefix as today), because
  capture names are the contract with grammar queries; unknown captures
  reject with `UnknownCapture`.
- **Resolution.** A key's color is the active theme's value, else the
  definition's default for the theme's appearance, following references.
  With no registered theme (before packages start, or when the selection is
  not registered or the package fails) the defaults alone apply, so there
  are no separate fallback themes. Today's built-in palette is dropped.
- **Contributions name color keys.** Wherever a contribution chooses a
  color, it names any color key: editor decoration and gutter tokens and
  tree badges become color-key strings. `"info"`, `"warning"`, `"error"`, and
  `"success"` are keys, and the badge's `"neutral"` becomes `"text.muted"`.
  A key with no definition yet paints in the surface's foreground, since
  its definition may arrive later. The shape stays fixed (a wavy underline,
  a gutter marker, a badge dot); line backgrounds and other decoration
  styles remain D114.
- **Selection is settings.** Native keys `theme.mode` (`"dark"`, `"light"`,
  `"system"`; default `"dark"`), `theme.dark` (default `"One Dark"`), and
  `theme.light` (default `"One Light"`). `system` follows the window
  appearance gpui reports. A name that is not registered falls back as
  above, so values never depend on package load order. The theme resolves
  when startup completes and on every selection change, replacing the
  `Theme` global and repainting every window; registry changes wait for the
  next selection change.
- **Select Theme lives in `@knot/themes`.** Settings are not persisted
  natively, so the package owns the picker and remembers the choice in its
  storage, writing it again in `start()` (the D091 rule: `post-init` still
  wins). The picker lists every registered theme. Choosing one sets
  `theme.<appearance>` to its name and, unless the mode is `system`, sets
  `theme.mode` to its appearance.
- **Bundled themes:** One Dark and One Light (the defaults), Catppuccin
  Mocha and Latte, Tokyo Night and Tokyo Night Day, Solarized Dark and Light.
  Each is ported from its published palette, using the upstream editor themes
  as the guide for which capture gets which color. Their MIT notices ship
  in the package.
- **Startup.** Bundled packages start before windows restore, so a theme
  registered in `start()` styles the first product frame.

## Exclusions and deferred decisions

- Live preview while moving through the picker: the picker has no
  selection-change callback yet. D071 adds one, and Select Theme then
  previews the highlighted theme and restores the previous one on dismissal.
- Theme-supplied icons and glyphs stay with D055.
- Fonts, sizes, and font features stay with D078 and D059; themes carry
  only colors and syntax font style and weight.
- Per-window or per-language themes.
- Theme families, inheritance by name, and importing VS Code or Zed theme
  files.
- Decoration styles beyond today's shapes (backgrounds, inline text) stay
  with D114.

## Checkpoints

### 1. Native theme model and themed surfaces ✅

Every native surface paints from color keys defined in one native table in
`src/app/theme.rs` (dark and light defaults, each a color or another key);
`theme::theme(cx)` returns the installed theme, else the dark defaults, and
`set_theme` repaints every window. A source-scan test keeps color literals
out of `src/app` outside `theme.rs`. The fallback syntax palette is
deliberately small until bundled themes land.

### 2. Theme registration API and selection settings ✅

`themes.register`/`get`/`list` and `themes.defineColor` live in
`src/app/themes.rs`, with the contract in `src/host/js/knot.d.ts` and the
guide's "Register a theme and define colors" section. The theme resolves
after both config phases and every package start, then again when the
`theme.*` settings or, in `system` mode, the window appearance change;
registering a theme later waits for the next selection change. An unknown
capture rejects with the `UnknownCapture` host error, because only native
code knows the vocabulary. gpui's test platform cannot change the system
appearance, so `system` mode is tested only against the reported one.

### 3. Contributions name color keys ✅

Editor decorations and gutter markers and tree badges name any color key
(`ColorKey` in `src/host/js/knot.d.ts`); see the extension guide's theme
section and the [semantic UI](../architecture/semantic-ui.md) layering
paragraph. Where decorations overlap, `error`, `warning`, and `info` paint
above other keys, most severe on top, and an undefined key paints as
interface text. Tree badge colors are covered only through the shared
`Theme::color` lookup, not a painted-color test.

### 4. `@knot/themes` with One Dark, One Light, and Select Theme ✅

[`extensions/@knot/themes`](../../extensions/@knot/themes/) registers One
Dark and One Light, ported from Atom's syntax and UI themes; themes share one
capture mapping over a palette of roles in `src/syntax.ts`, so later ports
supply colors only. UI keys Atom's themes do not map one-to-one (lists,
popovers, buttons, scrim, bright ANSI colors) were chosen for the port and
approved by manual review. `themes.select` persists its setting writes in
package storage and replays them in `start()`. The command's palette title is
its name, as for every extension command.

### 5. Remaining bundled themes and documentation 🚧 awaiting review

Deliverables:
- ✅ Catppuccin Mocha and Latte, Tokyo Night and Tokyo Night Light, Solarized
  Dark and Light, with their license notices in the package `LICENSE`.
  Tokyo Night's light variant is enkia's MIT Tokyo Night Light rather than
  tokyonight.nvim's Day, which is Apache-2.0. Solarized keeps its canonical
  sixteen terminal colors, whose bright colors are the base tones.
- ✅ Rationale in [decisions](../decisions.md#themes); behavior already in the
  [semantic UI](../architecture/semantic-ui.md#themes-and-color-keys) reference
  and the extension guide; D055's roadmap entry points at themes.
- After approval: this plan removed and the index item marked ✅ in a
  separate commit.

Validation: the package completeness tests cover the new themes; manual
review of each theme.

Review gate: final look at the themes and the documentation.
