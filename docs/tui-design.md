# TUI design system

The terminal session collects rows and form context for the active screen.
`ScreenModel::project` uses that in-memory data and the interaction state to
prepare calendar cells, agenda labels, graph views, timelines and picker rows.
`render::draw` consumes the projection and a `Theme`. Day maps the active entry to a display
row after inserting section headings, and Agenda stores the focused row.
The renderer receives neither the Vault nor the input `State` or `Keymap`.
It still owns widget layout and some display formatting. The model is private
to the TUI.

The `:` prompt parses text into a typed terminal command. View, timer,
settings, and proposal menus invoke the same commands as the prompt.
The edit key and `:edit` share the editor action. A connected session resolves
the selected UUID through the Daemon before opening the local editor. A local
Vault session resolves the source from its own snapshot. The default Linux
session starts with a service projection cache. Explicit external Vault and
non-Linux sessions start with a validated local snapshot.
Manual reload, changes reported by the Daemon, and service reconnection
invalidate the projection cache in connected sessions. The TUI polls the
change marker in the background. Focus is restored from the document or
occurrence identity. Explicit local sessions watch their own Markdown
files and reload the local snapshot.
The session dispatches the resulting action through the existing client or
local path. Text entry, resizing, and service responses follow their own
paths. The daemon receives typed business requests with explicit arguments.
Proposal confirmation also belongs to the terminal layer. The TUI and CLI
display the exact record before passing it to the queue for a locked recheck.

The Month model supplies the month title, grid labels, detail lines, compact
date labels and focused list row. A busy day keeps its `*` marker in both
layouts. The tall grid can also display `!` for a deadline on the same day.
The renderer selects the layout based on terminal size.

The Week model supplies the grid labels, compact day counts, selected cell,
and focused occurrence preview. In the compact layout the panel title shows
the selected hour. The occurrence title gets a line below the day counts
when space permits. When only one list row fits, it appears next to the
selected date. The renderer chooses the layout and the visible range of hour
rows. Enter opens the same occurrence in Day view.

Calendar views use the same bounded occurrence query. A multi-day recurring
event that starts before the visible range keeps its original span and document
identity. Month includes it in the day markers. Projection errors show their
cause before the source path so the reason fits in a short terminal. Date and
view navigation remain available.

`WorkflowScreenModel` prepares run labels, selected-run details in the
configured time zone, trace titles and viewport data, and contextual
footer actions. The footer only offers `Enter trace` when the selected run
has recorded steps. Trace wraps long lines by default and supports Page Up,
Page Down, Home and End. Press `w` to toggle wrapping and `H` or `L` to pan
the unwrapped text. Left and Right continue to switch steps. The approval
dialog projects its captured run and node,
escaped review text, and configured scrolling keys. Press `y` to submit or
Esc to cancel. If `y` is already bound to another action, the dialog displays
`Enter` for submission and preserves the configured binding. Both submission
keys require a fresh press.

`ProposalScreenModel` prepares list labels, detail titles and viewport data,
empty and unavailable messages, and contextual footer actions. Proposal
details wrap by default. Use `j`/`k`, arrows, Page Up or Page Down, and Home
or End to move through the wrapped preview. Press `w` to toggle unwrapped
inspection. Right or `l` also switches to unwrapped inspection and pans right.
The unwrapped view opens on the first visible source line. Left or `h` pans
back. Compact pending reviews keep approve, reject, and
return visible. Read-only reviews show navigation, and an empty list shows
refresh and return.

`GraphScreenModel` projects visible list rows, selected nodes,
adjacent dependencies and dependency edit previews. The TUI shares cached
nodes until a validated Vault snapshot replaces them. When the body is too
short for dependency headings, the list uses the full terminal width.
Graph dependency and Workflow reviews share a scrollable viewport with
grapheme-aware wrapping.
In Graph, `:graph add-dependency` and `:graph remove-dependency` share the
preview actions bound to `a` and `d`. They use the marked source WorkUnits and
the focused target.
In Graph review, use arrows or `j` and `k`, Page Up or Page Down, or Home and
End to read the preview. Enter or `y` applies the
edit and Esc or `n` closes the review.

`TimelineScreenModel` projects the two-week date labels, occurrence markers,
selected row and compact list labels. The renderer switches to the scrollable
list below 51 content columns or five content rows. Both layouts keep the
same date range, selection and navigation keys.

`OutlineScreenModel` projects hierarchy, focus and blocked state. Project
headings use the title style. The `!` marker precedes blocked WorkUnit titles
and the border carries a short legend. Both remain visible in monochrome.

`InboxScreenModel` puts the entry title first and styles the local capture
time and tags as secondary metadata. The project picker shows Escape first
in its footer and skips the assignment hint when no project is available.

Shortcut text is projected through the same `Keymap` used to normalize input.
The main footer, action menu, view selector, help overlay, and Workflow footer
reflect user-configured movement, menu, search, reload, help, quit, and view
keys. The main footer chooses a complete group using displayed column width,
including wide characters in configured keys. `Space` opens the scrollable
action menu. Home and End jump to its first and last entries, and Esc
returns to the current view. Direct keys work at each scroll position.
The selector opens from the main menu with `u`. It shows each view on
its own scrollable row, marks the current view, and keeps `Esc back` in the
footer. Press a displayed shortcut to switch directly. Escape returns to
the main menu.

The action menu opens Settings with Enter. The command prompt also accepts
`:settings`. General contains timezone and home view. Appearance contains
theme. On Linux, Daemon contains the Worker slot count. Tab and Shift+Tab
move through a single focus order across these
groups. Short terminals prioritize the focused field over the headings.
The form's field order drives navigation and the selected renderer row.
`SettingsScreenModel` projects the preferences form. The form keeps the
timezone as text and validates it when building `AppConfig` on save. Invalid
input remains visible and focus returns to the timezone field. Each visit
reads the current `config.toml` and parses it once to initialize the draft.
The platform applies shared defaults to omitted fields and compares the
captured text when saving. If disk changes while Settings is open, F5 reads
the latest file into the current page and preserves the focused field. This
replaces that page's unsaved draft after a successful read. The full-width
preferences footer shows F5 alongside the other page actions. An
unchanged draft preserves the file or its absence. The header shows when the
focused setting takes effect and how to edit it. The renderer applies the saved theme
immediately. Nonempty `NO_COLOR` keeps the session monochrome and the header
shows that `NO_COLOR` is active. Saving still updates the theme preference
in `config.toml`. The legacy `MCBTODO_TUI_THEME` variable is ignored.
The Worker slot count is limited to 1–4. Saving a change prompts the user
to restart the Daemon. Its startup flag `--worker-slots` overrides the saved
value.
The running Vault keeps its session timezone until restart. The save message
compares the saved preference with that active timezone, so it continues to
show the restart hint after later edits until the two values agree. When
launched with `--timezone`, Settings shows the override on the timezone field
and asks the user to restart without that flag if the saved value differs. Saving
`restore_last_view` also enables recording the current view at the next normal
exit. Esc discards the draft and returns to the action menu.
The startup policy reads `last-view` only when restoring. Startup falls back
to Month for a missing or unrecognized value. A read error opens Month with
a notice and leaves Settings available. Normal exit attempts to record the
view if restore is enabled and reports any write error after restoring the
terminal. Saved view identifiers and `:view` command arguments use the same
names.

If a readable `config.toml` is invalid at startup, the TUI opens Settings
before loading the Vault. Both entry points share the form's input and save
logic. The startup form starts with defaults and offers `Enter replace` and
`Esc exit`. The Vault opens after a successful
save using the repaired timezone or the explicit `--timezone` override.
If the file becomes invalid during a session, reopening Settings offers the
same replacement. In either case Escape preserves the original file and the
publisher detects edits made after the form opened. File read errors still
prevent repair. F5 also reloads the startup form when the file has been
edited externally. Enter continues with the new values.

F2 opens Keyboard from Settings and keeps the preferences draft in place.
The settings renderer shows the focused binding in the same row layout.
Preferences and Keyboard share the same platform configuration snapshot.
It binds the draft to its source file and compares both the parsed baseline
and the original file text at save time. An unchanged save preserves absent
files and existing comments.
Arrow keys and Tab select an action, typing a character updates the draft,
and Delete restores the default for that action. Enter validates and saves
`keymap.toml`, applies the keymap to input and shortcut hints, and returns to
preferences. Esc returns without applying the draft. The binding table
supplies default keys, action labels, and the scopes used for validation and
input normalization. TOML decodes each binding as a single Unicode character.
File loading and the Keyboard save both use the same checks for control
characters, conflicting bindings, and reserved keys. The current keymap keeps
unclaimed default keys as aliases. Ctrl+C is interpreted before printable key
bindings.
F5 reloads the focused Keyboard page independently of the preferences draft.

The TUI starts with default shortcuts and shows a notice if it cannot load
the optional keymap. A readable but invalid keymap opens the Keyboard page
with a default draft and an explicit `Enter replace` action. Escape leaves
the original bytes intact. When the keymap cannot be read, Keyboard opens in
info mode with the cause and a retry hint. F5 retries the read in place.
Escape returns to the preferences draft.

Preferences, shortcuts, Agent routing, and Codex parameters use the same
platform config writer. The writer compares the original file text, including
comment-only external edits.

F3 opens Agents and shows routes and grant counts from `agents.toml`. In a
policy with multiple providers, the selected row previews how many grants
would follow a new default. Enter saves that default. F5 reloads the policy
and focuses its default. On Linux, F6 edits the selected provider's endpoint
path. Enter saves, Escape discards the path draft, and F5 reloads the same
provider. A failed reload leaves the draft on screen. F4 opens the
`codex.toml` editor,
where Tab or arrow keys move between fields and Delete clears the focused
field. Enter saves and returns to Agents. An unreadable file offers F5 retry.
A readable invalid file can be replaced explicitly. Escape discards the draft
and returns to Agents. The preferences draft and focus are retained across
both pages. `doctor --json` supplies the optional installation diagnosis.

The timer menu puts Escape first. At 24 columns the calendar footer also
shows the time-block shortcut and all four timer keys. Other views show the
timer keys available there. Wider layouts show the action names.

The creation menu projects its available actions and displays a `CREATE`
mode badge. Arrow keys scroll through the shared viewport when space is
limited. Home and End reach the first and last entries. Action letters
open their forms from any scroll position, and Escape returns to the
previous menu.

## Visual hierarchy

- A presentation model supplies the header's view, date context, time zone
  and interaction mode. Short terminals use a single-line header. Narrow
  terminals reserve the mode badge for active operations.
- The body uses bordered panels and distinguishes the focused item from the
  selection range. Month switches to a scrollable date list when its grid
  cannot fit. If a panel has no interior space, keep its list, empty message
  or error text visible without a border.
- The feedback row preserves the latest operation or Vault error. After an
  invalid external edit the TUI keeps the previously validated snapshot.
  When vertical space runs out, the main layout drops its header and then
  operation feedback to keep a content row and shortcut footer alongside the
  Vault warning. At two rows with a Vault warning, content and the warning
  take both rows.
- Main-view footers offer shorter action groups in narrow terminals. Day,
  Agenda and Outline retain a visible Graph shortcut. `?` opens the full
  help overlay. Modal prompts show their own confirmation and dismissal keys.
- The creation form projects field labels, default values, focus, and the
  Enter action into `CreationScreenModel`. Each field occupies one row.
  Short terminals keep the focused row visible and narrow terminals retain
  the end of the active input, the cursor, and the primary actions.
  The Create menu and `:create` subcommands open the same forms. Project and
  all-day event forms default to the inclusive date selection. The deadline
  form defaults to the focused day. Task capture uses the selected Project
  and its observed revision.
- The deadline and time-block forms project their labels and actions. Both
  time-block menus and `:time-block` open the WorkUnit picker on the current
  date. On Linux in the default Vault, it uses the Daemon Outline projection.
  The picker and editor share the rows captured at open, so a background
  refresh cannot change the revision the user submits. Short
  layouts prioritize the active field, and the time-block picker reserves
  space for the selected WorkUnit. Narrow footers keep Enter and Esc visible.
- The Create menu and `:reminder` use the same reminder form. In a connected
  session, the Daemon Outline identifies the selected Task or WorkUnit for
  reminders and the selected WorkUnit for timers. A query error is displayed
  before either command proceeds.

`theme.rs` defines background, surface, active surface, border, focus border,
primary, secondary and muted text, accent, success, warning, error, selection,
and disabled tokens. The saved `tui_theme` selects the default, high-contrast,
or monochrome palette. A nonempty `NO_COLOR` selects monochrome, which retains
bold, reverse video, and selection symbols.

## Verification

The in-memory `TestBackend` tests check rendered cell positions and modifiers.
They cover mixed Chinese and Latin text, a long title clipped inside narrow
borders, cursor focus in monochrome, help and Vault error placement at low
height, visible date selection in the compact Month layout, and a five-row
Agenda with mixed Chinese and English text and a complete shortcut group. A
24×5 monochrome Agenda test preserves the focused entry and both status messages
when the list fits only without its panel. A second test at 24 columns verifies
that the selected mixed-language entry stays visible at heights of four, three,
and two rows while the surrounding rows adapt. A 96-column in-memory Graph test checks
the complete mixed-language title at five rows and dependency details at twelve.
The Workflow list distinguishes empty and unavailable states at 24×5
with concurrent feedback and a Vault warning.
Existing calendar and input tests cover navigation, selection, keymap overrides and
the compact Day footer. Week coverage checks compact counts and grid
alignment. An in-memory fixture verifies that the same mixed-language
overnight occurrence yields the original start, continuation marker and
focused preview. Creation-form fixtures check the selected date, feedback,
and save actions in 28×6 monochrome, plus a clipped Chinese and English
title and a visible cursor in high contrast. A 30×6 high-contrast Workflow
fixture asserts mixed-language cell alignment, focus, the right border, and
footer changes for waiting, unknown, and empty states. A wide-screen
assertion checks that a run with zero steps offers no trace shortcut.
The 24×5 monochrome Workflow approval fixture checks the panel boundary,
remapped scrolling, confirmation and Escape. A separate fixture checks the
`Enter` fallback when `y` is configured for movement.
Timeline fixtures verify the start `>>` and continuation `==` markers for an
overnight occurrence and preserve the document focus when moving dates. The
30×6 monochrome buffer test checks Chinese column alignment, long-title
clipping, selection, the right border and the footer.
`shortcut_hints_follow_the_same_keymap_as_input` verifies that configured
movement, menu, help, reload, quit, picker, and view keys appear in the
corresponding footer, menu, help, and Workflow states.
The Month footer test uses Chinese key labels at 25, 30 and 80 columns and
checks that the selected hint fits. The 24×5 menu test checks the mode, right
border, footer, Home and End, and direct access to proposals from the end.
The 28×6 in-memory proposal fixture checks mixed Chinese and English content
and exact footer text for empty, pending, and read-only states.
The 24×5 detail fixture checks mixed-language wrapping between panel borders,
access to the last content row, return to the start, horizontal panning,
and approval after navigation.
The shared Proposal and Workflow trace viewport anchors the first visible
grapheme in the wrapped text across terminal resizes. Explicit navigation
sets a new anchor. Returning to wrapped mode follows vertical movement made
while unwrapped. Proposal fixtures verify a mixed-language source line at
24×5 and 70×5, and a long line through repeated resizes from 24×5 through
28×5, 88×5 and 24×12. End establishes a new anchor at the last source line.
The 24×5 trace fixture checks the step label, the continuation row between
both borders, the final row, horizontal panning, the next-step request
and Escape back to the run list.
Graph tests verify the selection shortcuts in 30×6, and verify that the
edge and blocking-state preview can be read in 28×6 before applying or
cancelling a dependency edit.
Help shares the review viewport. In 28×6, the End key reaches the last
shortcut, Home returns to the start, and closing help restores the view.
The 28×5 modal cases verify deadline input and actions, a matching Chinese
WorkUnit in the time-block picker, the focused end time and cursor, and the
complete confirmation and Esc actions.
Outline fixtures cover a 23×5 monochrome terminal, the focused WorkUnit marker,
wide-character alignment and source identity. The in-memory high-contrast
fixture also checks project headings, indentation and the empty state.
The 24×5 Inbox fixtures check the selected mixed-language title and right
border, focus and source identity, Escape and Enter in the project picker,
and the available actions when either list is empty.
The 24×5 timer menu fixture checks Escape and the available actions for
calendar and non-calendar views. It also verifies that the calendar row and
right border remain visible under the menu.
The 24×5 creation menu fixture checks that Home and End bring the first
and last actions into view inside the border. It exercises returning with
Escape and opening the time-block picker by its existing shortcut.
The 24×5 monochrome view selector fixture checks the mode badge, current
view label, panel border, and Escape footer. It scrolls through all nine
view shortcuts, then checks Home, End, return to the main menu, and direct
entry into Workflow.
`cargo xtask smoke` exercises the real TUI under a pseudoterminal. Its
development-only `vt100` parser reconstructs the current screen across
incremental cursor updates. The startup fixture uses an isolated workspace
and an empty `PATH`. A second process uses a 28×6 pseudoterminal and
`NO_COLOR=1`. It opens Help from the compact Month view, sends the terminal
sequences for End and Home, checks the corresponding text on screen, and
returns to the selected date. It then checks the first and last actions in
the menu and returns to the selected date before quitting.

Visual fixtures for every modal and complete extraction of view formatting
remain part of Phase 4. See the [architecture assessment](architecture-assessment.md).
