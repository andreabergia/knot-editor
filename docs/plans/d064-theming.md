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
- **A closed native key vocabulary**, validated at registration. Unknown UI
  keys and unknown capture names reject with `TypeError`, so typos fail
  loudly; syntax keys are capture names (resolved by longest dotted prefix as
  today) and never decoration names. The vocabulary starts small, derived
  from what surfaces draw today (editor, tabs and panes, docks, trees,
  pickers and transient fields, completion, prompts, terminal and its 16 ANSI
  colors, and `error`/`warning`/`info`, which also color diagnostic
  underlines). Later surfaces (palette, gutter, status bar, Find) add their
  keys when they land.
- **Missing keys fall back** to a minimal native theme of the same
  appearance. Native code keeps one dark and one light fallback, which also
  apply when no registered theme matches the selection or the package fails.
  Today's built-in palette is dropped.
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
  selection-change callback (D071 may add one).
- Theme-supplied icons and glyphs stay with D055.
- Fonts, sizes, and font features stay with D078 and D059; themes carry
  only colors and syntax font style and weight.
- Per-window or per-language themes.
- Theme families, inheritance by name, and importing VS Code or Zed theme
  files.

## Checkpoints

### 1. Native theme model and themed surfaces

Deliverables:
- `Theme` holds an appearance, a color for every UI key, and the resolved
  highlight styles. A minimal native dark and light theme define every key.
  Dark is the active default, so the app looks different from today but
  coherent.
- Every native surface reads the `Theme` global: product shell render, tab
  drag, tree view, explorer tree, transient interactions, command palette,
  completion, text input, editor, terminal view and ANSI defaults, and the
  personal-config error window. No color literals remain outside `theme.rs`.
- Replacing the global repaints every window.

Validation: unit tests that the key vocabulary is sorted and unique and that
both fallbacks define every key; a source-scan test that no `rgb(0x`,
`rgba(0x`, or `hsla(` literal appears in `src/app` outside `theme.rs`;
existing editor and terminal tests pass. Manual look at every surface.

Review gate: the key vocabulary and how surfaces map onto it.

**Implementation context:** `src/app/theme.rs` (today `Theme::dark()`,
`with_styles`, `Highlight` vocabulary; editor paint already reads it via
`cx.global::<Theme>()` in `src/app/editor/paint.rs` and copies colors per
paint). Color literals live in `src/app/product/render.rs`,
`src/app/tree_view.rs`, `src/app/transient.rs`, `src/app/command_palette.rs`,
`src/app/completion.rs`, `src/app/text_input.rs`, `src/app/workspace_tree.rs`,
`src/app/product/tab_drag.rs`, `src/app/personal_config_error.rs`,
`src/app/terminal/view.rs`, and the default palette in
`src/app/terminal/colors.rs`. `editor.rs` installs the global with
`default_global`; keep one installation point at application startup.

### 2. Theme registration API and selection settings

Deliverables:
- A native theme registry mirroring `LanguageRegistry`: lifecycle-owned
  registrations, latest name wins, teardown reveals the previous one.
- Host operations `registerTheme`, `unregisterTheme`, `listThemes`, `getTheme`,
  with typed decoding and `TypeError` for invalid keys, colors, and styles.
- Native settings `theme.mode`, `theme.dark`, `theme.light`; the effective
  theme recomputes on any registry change, settings write, or window
  appearance change (`system`), resolving missing keys from the fallback of
  the theme's appearance.
- `knot.d.ts` declarations with `ThemeColorKey` as a string-literal union,
  the fake host's `themes` implementation, and the extension guide section.

Validation: registry unit tests (override, teardown reveal, missing-key
fallback, unknown name falls back); product tests in
`src/app/product/tests/themes.rs` for registration through the host,
validation errors, settings-driven switching, teardown during use, and
`system` mode following a changed appearance where gpui's test platform
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

### 3. `@knot/themes` with One Dark, One Light, and Select Theme

Deliverables:
- A bundled `extensions/@knot/themes` package that registers One Dark and One
  Light in `start()`, plus its MIT notice.
- A Select Theme command: a picker over `themes.list()`, the setting writes
  described above, and the choice persisted in package storage and applied
  again in `start()`.

Validation: package tests under `node:test` that every bundled theme defines
every UI key and only known captures, and that selecting a theme writes the
settings and survives a restart of the package; a product test that the
bundled defaults are active after startup.

Review gate: how the ported themes look across every surface; adjust the key
vocabulary or port mapping here before porting the rest.

**Implementation context:** copy the layout of `extensions/@knot/languages`
(`knot.jsonc`, `src/extension.ts`, `test/`, `tsconfig.json`); `build.rs`
picks up every directory under `extensions/@knot`. Picker and storage usage
to mirror: `extensions/@knot/projects/src/commands.ts` (`workbench.showPicker`)
and `state.ts` (`storage.read`/`write`).

### 4. Remaining bundled themes and documentation

Deliverables:
- Catppuccin Mocha and Latte, Tokyo Night and Tokyo Night Day, Solarized Dark
  and Light, with their license notices.
- Durable behavior moved into the architecture references and rationale
  into `docs/decisions.md` (theme ownership, closed vocabulary, native
  fallback, selection settings, no `extends`); D055's roadmap entry updated
  to point at themes; this plan removed and the index item marked ✅.

Validation: the package completeness tests cover the new themes; manual
review of each theme.

Review gate: final look at the themes and the documentation.
