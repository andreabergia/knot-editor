# D064 — Theming system

Status: 🚧 approved, not started. Item of the
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
  reject with `TypeError`.
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
  above, so values never depend on package load order. Any change to the
  settings or the registry replaces the `Theme` global and repaints every
  window.
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

### 2. Theme registration API and selection settings

Deliverables:
- A native theme registry mirroring `LanguageRegistry`: lifecycle-owned
  registrations, latest name wins, teardown reveals the previous one.
- Host operations `registerTheme`, `unregisterTheme`, `listThemes`,
  `getTheme`, and `defineColor`, with typed decoding and `TypeError` for
  invalid colors, captures, and styles. Extension color definitions join the
  native table, are removed at teardown, and re-resolve the active theme.
- Native settings `theme.mode`, `theme.dark`, `theme.light`; the effective
  theme recomputes on any registry change, settings write, or window
  appearance change (`system`), resolving missing keys from the definition
  defaults of the theme's appearance.
- `knot.d.ts` declarations, with native color keys as an interface packages
  extend by declaration merging like `Settings`; the fake host's `themes`
  implementation; and the extension guide section.

Validation: registry unit tests (override, teardown reveal, missing-key
defaults, unknown name falls back, extension definitions and references);
product tests in
`src/app/product/tests/themes.rs` for registration through the host,
validation errors, duplicate definitions, settings-driven switching,
teardown during use, and `system` mode following a changed appearance where gpui's test platform
allows it; a fake-host test that the fake's key list matches the native one,
like `fake_host_lists_every_capture_name`.

Review gate: the public API shape and selection semantics.

**Implementation context:** mirror `src/app/languages.rs`
(`LanguageRegistry`, `ApplicationLanguages` global, `register`/`unregister`/
`remove_lifecycle`) in a new `src/app/themes.rs`, or fold the registry into
`theme.rs` if it stays small; declare it in `src/app/mod.rs`. Handlers sit
beside `register_language` in `src/app/extensions/host/handlers.rs`; teardown
is wired next to `languages::remove_lifecycle` in
`src/app/extensions/host.rs` (~line 2225). JS glue mirrors `registerLanguage`
in `src/host/js/bootstrap.js` and the `languages` export in
`src/host/js/knot.js`. Native settings are defined in `src/app/settings.rs`
next to `EDITOR_TAB_SIZE`; observe writes there to recompute. Window
appearance comes from gpui's `Window::appearance` and
`observe_window_appearance`. Fake host: `scripts/extensions/testing/host.ts`.
Docs: `docs/architecture/semantic-ui.md#syntax-highlighting-and-languages`
(theme ownership), `docs/extension-api.md`, `docs/schemas/knot.schema.json` if
it lists API namespaces.

### 3. Contributions name color keys

Deliverables:
- `EditorContribution.decoration` and `gutter` and `TreeItem.badge` take
  any color key string instead of the severity tokens; the editor paints
  decorations and gutter markers, and trees paint badges, in the resolved
  key's color, or the surface foreground for an undefined key.
- Declarations, the fake host, examples, and bundled packages updated.

Validation: product tests that a contribution naming an extension-defined
key paints in that key's color, follows a theme switch, and falls back for
an undefined key; existing highlight and tree tests updated.

Review gate: the contribution contract.

**Implementation context:** `SemanticToken` and `TreeBadge` in
`src/host/js/knot.d.ts`; their decoders in the engine's marshaling module
and `src/app/extensions/semantics.rs`; editor decoration layering through
`Highlight` decoration names in `src/app/theme.rs` and
`src/app/editor/layout.rs`/`paint.rs`; badge painting in
`src/app/tree_view.rs`; highlight tests in
`src/app/extensions/host/highlight_tests.rs`.

### 4. `@knot/themes` with One Dark, One Light, and Select Theme

Deliverables:
- A bundled `extensions/@knot/themes` package that registers One Dark and One
  Light in `start()`, plus its MIT notice.
- A Select Theme command: a picker over `themes.list()`, the setting writes
  described above, and the choice persisted in package storage and applied
  again in `start()`.

Validation: package tests under `node:test` that every bundled theme sets
every native color key and only known captures, and that selecting a theme writes the
settings and survives a restart of the package; a product test that the
bundled defaults are active after startup.

Review gate: how the ported themes look across every surface; adjust the
native keys or port mapping here before porting the rest.

**Implementation context:** copy the layout of `extensions/@knot/languages`
(`knot.jsonc`, `src/extension.ts`, `test/`, `tsconfig.json`); `build.rs`
picks up every directory under `extensions/@knot`. Picker and storage usage
to mirror: `extensions/@knot/projects/src/commands.ts` (`workbench.showPicker`)
and `state.ts` (`storage.read`/`write`).

### 5. Remaining bundled themes and documentation

Deliverables:
- Catppuccin Mocha and Latte, Tokyo Night and Tokyo Night Day, Solarized Dark
  and Light, with their license notices.
- Durable behavior moved into the architecture references and rationale
  into `docs/decisions.md` (theme ownership, color definitions and native
  defaults, open theme maps, selection settings, no `extends`); D055's roadmap entry updated
  to point at themes; this plan removed and the index item marked ✅.

Validation: the package completeness tests cover the new themes; manual
review of each theme.

Review gate: final look at the themes and the documentation.
