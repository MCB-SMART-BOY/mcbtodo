# TUI Design and Verification Workflow

Use this workflow when changing TUI rendering, screen state, keymaps, layout,
theme, status messages, dialogs, or terminal compatibility. Preserve current
business and interaction semantics while separating presentation from data and
behavior.

## Presentation Boundary

Project a Vault or Application snapshot into a pure in-memory `ScreenModel` or
presenter result. A Ratatui renderer receives that model, theme, and metrics and
only draws it. Keep Vault data access, input `State`, `Keymap`, business
queries, workflow state inference, and complex status decisions in the owner
or projection layer.

Keep domain identity and occurrence identity distinct. A repeated calendar row
must retain the selected object and occurrence date/time. Shared screen data may
serve future GUI or web clients, while terminal coordinates and Ratatui styles
remain presentation details.

## Design System

Centralize theme tokens for background, surface, active surface, border, focus
border, primary text, secondary text, muted text, accent, success, warning,
error, selection, and disabled state. Centralize layout metrics and reusable
components. Do not duplicate colors, border rules, or spacing in each renderer.

Render the current title and context first, then mode and focus, content hierarchy
and whitespace, dialogs, empty/loading/error states, footer hints, narrow layouts,
Unicode width behavior, no-color output, and high-contrast output. Footer hints
show only the current context's three to five most relevant actions. Full help
belongs in the help overlay or command palette. Key labels must be projected from
the same `Keymap` used for input whenever remapping is supported.
Give the help overlay a scrollable viewport. Test reaching the last shortcut
in a short terminal and returning to the same view and focus.

Fit main-view shortcut groups by terminal columns, including wide characters
in remapped key labels. Select a complete shorter group when space is limited.
Use the shared menu viewport for the main action, view and creation menus.
Check that Home, End and Esc work in a 24×5 monochrome buffer and that direct
action keys work from any scroll position. Include menu navigation in the
28×6 pseudoterminal smoke.

Keep one preferences form in input state and project it through
`SettingsScreenModel`. Build `AppConfig` at save time. Derive General,
Appearance, and Linux Daemon headings from the field order and available height. Keep input
and rendering on that same field order. Test that
reverse focus crosses groups and that the headings appear in order at 64×14.
Keep only the timezone's
editable text as transient input state. On a failed save from another
field, focus the timezone and keep the entered text. Keep
configuration reads and writes in the platform layer. Use
`AppConfig::default()` for omitted preferences, with the same defaults in the
TUI and other entry points. Test empty and partial files, invalid values, and
unrecognized keys separately. Verify that an invalid timezone and Esc leave
the saved configuration untouched. Verify that a save rejects intervening
changes, including comment-only edits, and reopening Settings reads the new
values. An unchanged draft preserves file bytes or absence. If a manual edit
breaks a readable config during a running session, check the explicit replace
action at 24×5, cancellation, a subsequent conflict, and a successful repair.
At 24×5, check the focused value,
validation error, and save and cancel hints. Verify that focus follows the
theme row at the same size. Start the PTY smoke with a partial config and
`--timezone`. Verify the defaults, the focused field's apply timing, and the
timezone override notice. Save the preferences, including Worker slots on
Linux. Check the restart hints and restart the TUI without `--timezone`. Compare the bytes of
`agents.toml`, `codex.toml`, and `keymap.toml` before and after the save.
Exercise successive saves while the session still uses the old timezone.
The restart hint persists after saving another field or an unchanged draft,
and clears when the saved timezone matches the active Vault.
Use one view-name mapping for the `:view` command and `last-view`. Verify
that the Month startup policy skips the state file and that restore falls
back to Month for an absent or unrecognized saved value. With an unreadable
saved view, verify that Month opens with a notice, quit succeeds, and Settings
can disable restoration. Record the view on normal exit.
Check the palette on the rendered screen before restarting to verify that a
successful save applies the theme in the current session. Exercise startup
with the obsolete `MCBTODO_TUI_THEME` set to an invalid value. Restart with
`NO_COLOR` and verify its hint, a changed preference on disk, and the
monochrome palette after saving.

Open Keyboard with F2. Use the same action table for defaults, edit rows,
validation scopes, and input normalization. Keep the preferences draft while
editing shortcuts. Use the platform `ConfigSnapshot` for both settings pages.
It owns change detection and uses the same file comparison and publication
path for both files. Verify that an unchanged keymap save keeps an absent file
absent and preserves comments in an existing file. Test saving and discarding
`keymap.toml`, a duplicate binding, a concurrent edit, and the focused row in
a 24×5 buffer. Exercise the production configuration reader for malformed
bindings and symlinks. Round-trip a single Unicode character through the
same snapshot used by Keyboard. Verify
the saved key in both input and Help, and check that Ctrl+C still exits
when bare `c` is remapped. In the PTY smoke, save a binding, use it in the
same session and after restart, and compare the other config files with
their pre-save bytes.
Start the TUI with an invalid optional keymap and verify that the startup
notice and default input are usable. Check that Keyboard advertises replacing
the invalid file, Escape preserves it, and an intervening edit still blocks
publication. Exercise the repair and subsequent restart in the PTY smoke.

Open Agents with F3. Project the shared `doctor` discovery result without
performing I/O in the renderer. Use PTY smoke to open Agents with a
modified preferences draft, return with Escape, and save the original draft.
With multiple configured providers, verify the selection previews the inherited
grant count and Enter saves that provider as the default. Exercise the changed
route and unchanged explicit grants through the policy reader.
Use F6 to edit the selected socket path. Cover cancellation, invalid paths,
and comment-only conflicts. Exercise the route with the saved policy reader
and check the other provider and exact revision grants.

Use the same rule for the proposal queue. Project empty, list, pending detail
and read-only detail states before rendering. Keep the primary actions and
return visible in the compact pending footer. Test that empty and read-only
states only advertise the actions that apply to their content.
Wrap proposal detail at grapheme boundaries by default. At 24×5, test that
Page Down reaches the continuation of a mixed-language line, End reveals the
last content row, and Home restores the first row. Preserve unwrapped
inspection with `w` and horizontal panning with arrows or `h`/`l`.
Verify both borders and the pending actions at that size.
Resize the wrapped preview from 24×5 to 70×5 and check that the same
source line stays visible. Move to a different source line in unwrapped
mode, switch back to wrapping, and verify that the line remains visible
after narrowing the terminal again.
Within a long wrapped source line, keep the current grapheme in view
across repeated resizes, including a width that removes wrapping and a
height that displays the entire preview. Verify that End sets a new anchor.
Use the same viewport for Workflow trace. In 24×5, verify the step label,
wrapped output, the final row, horizontal panning and Escape. Preserve
Left and Right for adjacent step requests and `H` and `L` for panning.

Project the Graph list and dependency edit from the installed Vault snapshot.
Refresh the cached nodes when the validated snapshot changes and format only
visible rows. At 96×5, give the Graph list the full width and check that the
focused mixed-language title fits inside the panel. At 96×12, verify that the
same focus shows prerequisite and dependent details.
Graph and Workflow confirmations share the review viewport.
Test scroll, confirmation, cancellation, and access to the complete preview
in a short terminal.
For Workflow approval, check the review snapshot and configured movement
labels in 24×5 monochrome. Use `y` to submit, or `Enter` when `y` is already
assigned in the keymap. Verify that the configured key still moves the preview
and that holding the submission key never counts as a fresh press.
Capture the approval review identifier from the same run summary as the
displayed text. Submit that identifier even if the run list refreshes while
the modal is open. When the Daemon reports a review conflict, require a new
inspection and confirmation.

## Required Test Matrix

The Month projection supplies concise date and marker labels for both the
grid and the compact list, the detailed cell line and the focused list row.
Test simultaneous busy and deadline entries without a Vault. At compact
sizes, check that the same selected date remains visible inside the panel.

Week projects the selected hour and occurrence title. Verify daily counts
and the focused preview at 42×18. At 24×5 in monochrome, keep the preview
beside the selected date, check Chinese alignment and the right border, and
verify that Enter opens the previewed occurrence.

Use the bounded calendar projection for Month, Week, Day, Agenda, and
Timeline. Verify that a multi-day recurring event can retain its original
span and source across a month boundary. When projection exceeds the
per-rule bound, show the reason in a 24×5 buffer and leave navigation
available. Compare snapshot source paths and revisions for watch refresh
so an edit to a recurring template is detected regardless of the active view.
For a timed recurring event during a DST fold, show both offsets when
the local end time precedes the local start time. Check that Day and Agenda
retain the same occurrence through reload and resolve it to the template.

The deadline and time-block forms prioritize the active input and dismissal
action in a 28×5 terminal. Test that the time-block picker keeps a matching
WorkUnit visible, the document picker keeps a Chinese match visible, and the
editor retains the current time and cursor. In a 28×4 monochrome terminal,
verify focus, validation feedback, and the dismissal action together.
Preserve the existing revision capture and Esc transitions.

The Outline model projects hierarchy, focus and blocked state. Show the
blocked marker before the WorkUnit title and label it in the border legend.
Verify the marker, focused source, and Chinese cell alignment in a 23×5
monochrome terminal. Use in-memory rows to check indentation, project headings
and empty state in high contrast.

Keep Inbox titles visible before capture times and tags when rows are clipped.
For the Linux default Vault, exercise Inbox and Project rows that are absent
from the client snapshot. Submit the Inbox revision seen when the picker
opened and verify that a stale revision prompts a reload. The systemd smoke
assigns an entry in one TUI and opens the resulting Task in a second TUI.
Use a 24×5 fixture to verify the title, selected source, border, and project
picker controls. Empty Inbox and project lists show only applicable actions.

Keep Escape first in the timer menu. Check in 24×5 that the calendar shows
the block shortcut and timer controls, the non-calendar menu shows the timer
controls, and the calendar row still fits between its borders.

Use the scrollable viewport for the projected creation menu. In 24×5, check
Home and End, the right border, Escape back to the previous menu, and direct
access to the time-block picker with its shortcut.

Give the view selector one row per view in the same viewport. Show the current
view, keep Escape back in the footer, and verify that every shortcut can be
read in 24×5 monochrome. Configured shortcuts must open their view directly
from any scroll position.

Test normal, narrow, and short terminals. Include Chinese, mixed Chinese and
English, wide Unicode characters, long titles, truncation, selection, focus,
dialogs, errors, no-color terminals, and high contrast themes. Assert visible
boundaries, text positions, clipping, overlap, and alignment. A no-panic test is
not sufficient.

When a panel has no interior space, prioritize the list item, empty state
or error text. At 24×5 in monochrome, verify that Agenda keeps the focused
entry and selection mode visible alongside action feedback, shortcuts and
a Vault warning. Verify that Workflow keeps its empty or unavailable message
visible with the same chrome and offers Refresh in both states.
With a warning and operation feedback present, verify the focused Agenda
entry at 24 columns and four, three, and two rows. The main layout drops
its header, then feedback, then the shortcut footer as height decreases.

Keep visual changes separate from storage and IPC migrations. Preserve calendar
occurrence semantics, mode transitions, navigation, approval prompts, and other
existing key behavior unless the change explicitly updates the contract and its
help text.

For Linux default-Vault Task capture and Project, Deadline, all-day event,
one-time reminder, and time block creation, exercise the Daemon path with
`cargo xtask smoke-systemd`. Keep the draft open when the service is absent.
After an ambiguous response, report the submitted UUID and, for time blocks,
the submitted interval. Require the user to check the Vault before retrying.
Check that stale WorkUnit revisions are rejected and a successful save focuses
the new occurrence. Cover foreground behavior for supported external-Vault
forms. Reminder creation remains limited to the default Vault. Verify its
Inbox entry without requiring desktop notifications.

Read the saved Project dates, all-day event and WorkUnit block back from a
separate CLI process with `calendar --daemon`. Query both days of the
cross-month event and the first day after it. Open a new TUI on the
continuation date and check that Day renders the event through the same
service. With the service stopped, verify that the calendar shows the start
hint and navigation works. Exercise an external Vault in the offline
terminal smoke. Use the same date-window response for drawing and selection.
On refresh, keep the exact selected occurrence if it is still present.

Open a second TUI after capturing a Task and confirm the saved title appears
in Outline. Use the same Outline response for drawing, selection and the
Project revision in the Task form. Keep the draft open on a stale revision.
Verify that a truncated multi-frame response does not publish partial rows.
Check that global search can find a document by title and source path from the
same response and focus it in Outline even when the local snapshot lacks it.
Check the startup hint and Esc dismissal when the service is offline.

## Verification

Run focused TUI tests and the complete workspace checks.

```sh
cargo test --locked -p mcbtodo --bin mcbtodo tui::
cargo fmt --all -- --check
git diff --check
cargo xtask check
cargo run --locked -- --vault examples/vault --timezone Asia/Shanghai validate
```

When snapshot infrastructure exists, review the rendered snapshots at the
specified terminal sizes. If a terminal capability or font renderer cannot be
verified in the current environment, record the limitation and keep the status
partial.
